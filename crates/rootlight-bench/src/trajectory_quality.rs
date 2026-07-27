//! Candidate-bound quality evidence for preregistered agent workflows.
//!
//! The retained artifact contains task, fixture, candidate, score, and
//! efficiency digests only. Prompts, answer keys, responses, and source bytes
//! remain outside this module and are consumed before measurements enter it.

use std::{collections::BTreeMap, fmt::Write as _};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{
    MAX_QUALITY_LOSS_CENTIPOINTS, TrajectoryAttemptOutcome, TrajectoryCondition,
    TrajectoryEvidencePackage, TrajectoryProtocol, TrajectoryWorkflowFamily,
};

/// Schema for retained all-workflow quality evidence.
pub const WORKFLOW_QUALITY_SCHEMA_VERSION: &str = "rootlight.agent-workflow-quality/1";
/// Stable implementation identity bound into every quality protocol.
pub const WORKFLOW_QUALITY_RUNNER_ID: &str = "rootlight-workflow-quality-runner-v1";
/// Maximum encoded size accepted for one quality artifact.
pub const MAX_WORKFLOW_QUALITY_EVIDENCE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum score on the zero-to-one-hundred quality scale.
pub const MAX_WORKFLOW_QUALITY_SCORE_CENTIPOINTS: u16 = 10_000;

const MAX_LABEL_BYTES: usize = 160;
const EXPECTED_WORKFLOW_COUNT: usize = 14;

/// Closed dimensions used to grade every workflow candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowQualityDimension {
    /// Factual and semantic correctness.
    Correctness,
    /// Coverage of required task facts.
    Completeness,
    /// Support for claims from observed evidence.
    EvidenceSupport,
    /// Honest handling of partial or uncertain results.
    UncertaintyHandling,
    /// Usefulness for the requested next action.
    Actionability,
    /// Relevance of selected source evidence.
    SourceRelevance,
    /// Adherence to the exact preregistered task.
    TaskAdherence,
}

impl WorkflowQualityDimension {
    const ALL: [(Self, u16); 7] = [
        (Self::Correctness, 2_500),
        (Self::Completeness, 2_000),
        (Self::EvidenceSupport, 2_000),
        (Self::UncertaintyHandling, 1_000),
        (Self::Actionability, 1_000),
        (Self::SourceRelevance, 1_000),
        (Self::TaskAdherence, 500),
    ];
}

/// One preregistered prompt and held-out-answer commitment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowQualityTaskRegistration {
    /// Workflow identity from the trajectory protocol.
    pub workflow_id: String,
    /// Zero-based attempt index.
    pub attempt_index: u16,
    /// SHA-256 of the exact prompt supplied to both candidates.
    pub prompt_sha256: String,
    /// SHA-256 commitment to the held-out deterministic answer key.
    pub answer_key_sha256: String,
}

/// Source-free task instance retained before candidate execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowQualityTaskProtocol {
    /// Workflow identity from the trajectory protocol.
    pub workflow_id: String,
    /// Closed workflow family.
    pub family: TrajectoryWorkflowFamily,
    /// Digest of the trajectory task definition.
    pub task_sha256: String,
    /// Exact fixture-tree digest.
    pub fixture_sha256: String,
    /// Zero-based attempt index.
    pub attempt_index: u16,
    /// Deterministic attempt seed.
    pub deterministic_seed: u64,
    /// Digest of the exact prompt shared by both candidates.
    pub prompt_sha256: String,
    /// Commitment to the held-out answer key unavailable to adapters.
    pub answer_key_sha256: String,
}

/// Complete all-workflow grading protocol frozen before execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowQualityProtocol {
    /// Stable experiment identity.
    pub experiment_id: String,
    /// Canonical source revision under test.
    pub source_revision: String,
    /// Digest of the complete trajectory protocol.
    pub trajectory_protocol_sha256: String,
    /// Digest of all trajectory workflow definitions.
    pub trajectory_tasks_sha256: String,
    /// Exact fixture-tree digest.
    pub fixture_sha256: String,
    /// Digest of the fixed seven-dimension rubric.
    pub rubric_sha256: String,
    /// Immutable per-workflow loss limit.
    pub maximum_quality_loss_centipoints: u16,
    /// Source-free task instances in canonical workflow/attempt order.
    pub tasks: Vec<WorkflowQualityTaskProtocol>,
}

/// Earned points for one fixed rubric dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowQualityDimensionScore {
    /// Closed rubric dimension.
    pub dimension: WorkflowQualityDimension,
    /// Candidate points earned within this dimension.
    pub earned_centipoints: u16,
    /// Fixed maximum contribution of this dimension.
    pub maximum_centipoints: u16,
}

/// Ephemeral caller measurement for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowQualityCandidateMeasurement {
    /// Candidate condition.
    pub condition: TrajectoryCondition,
    /// Attempt identity from retained trajectory evidence.
    pub attempt_id: String,
    /// Digest of the candidate answer and observed-evidence inventory.
    pub candidate_sha256: String,
    /// Deterministic held-out rubric result.
    pub dimensions: Vec<WorkflowQualityDimensionScore>,
}

/// Ephemeral paired measurement for one task instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowQualityPairMeasurement {
    /// Workflow identity from the preregistered task.
    pub workflow_id: String,
    /// Zero-based attempt index.
    pub attempt_index: u16,
    /// Rootlight candidate measurement.
    pub rootlight: WorkflowQualityCandidateMeasurement,
    /// Task-driven bounded-file candidate measurement.
    pub bounded_file_exploration: WorkflowQualityCandidateMeasurement,
}

/// Source-free candidate grade bound to a retained trajectory attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowQualityCandidate {
    /// Candidate condition.
    pub condition: TrajectoryCondition,
    /// Attempt identity from retained trajectory evidence.
    pub attempt_id: String,
    /// Digest of the ephemeral answer and evidence inventory.
    pub candidate_sha256: String,
    /// Weighted total quality score.
    pub score_centipoints: u16,
    /// Complete fixed-dimension score vector.
    pub dimensions: Vec<WorkflowQualityDimensionScore>,
    /// Actual tool calls; reported alongside and never applied to quality.
    pub tool_calls: u32,
    /// Actual request-plus-response tokens under pinned `o200k_base`.
    pub o200k_tokens: u64,
}

/// One retained Rootlight/bounded-files quality pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowQualityPair {
    /// Workflow identity.
    pub workflow_id: String,
    /// Closed workflow family.
    pub family: TrajectoryWorkflowFamily,
    /// Digest of the trajectory task definition.
    pub task_sha256: String,
    /// Exact fixture-tree digest.
    pub fixture_sha256: String,
    /// Zero-based attempt index.
    pub attempt_index: u16,
    /// Deterministic attempt seed.
    pub deterministic_seed: u64,
    /// Digest of the exact shared prompt.
    pub prompt_sha256: String,
    /// Commitment to the held-out answer key.
    pub answer_key_sha256: String,
    /// Rootlight grade and efficiency.
    pub rootlight: WorkflowQualityCandidate,
    /// Task-driven bounded regular-file grade and efficiency.
    pub bounded_file_exploration: WorkflowQualityCandidate,
    /// Non-negative Rootlight loss against the bounded baseline.
    pub rootlight_quality_loss_centipoints: u16,
    /// Whether this pair stays within the immutable loss threshold.
    pub within_threshold: bool,
}

/// Per-workflow aggregation over all preregistered attempt seeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowQualitySummary {
    /// Workflow identity.
    pub workflow_id: String,
    /// Number of expected seed pairs.
    pub expected_pairs: u32,
    /// Number of retained quality pairs.
    pub observed_pairs: u32,
    /// Mean Rootlight quality score.
    pub rootlight_mean_quality_centipoints: u16,
    /// Mean bounded-file quality score.
    pub bounded_file_mean_quality_centipoints: u16,
    /// Non-negative loss between the two workflow means.
    pub rootlight_quality_loss_centipoints: u16,
    /// Largest loss observed for any seed pair.
    pub maximum_pair_loss_centipoints: u16,
    /// Whether both mean and every pair satisfy the fixed limit.
    pub within_threshold: bool,
}

/// Complete quality denominator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowQualityDenominator {
    /// Workflows required by the protocol.
    pub expected_workflows: u32,
    /// Workflows with complete retained summaries.
    pub observed_workflows: u32,
    /// Seed pairs required by the protocol.
    pub expected_pairs: u32,
    /// Seed pairs retained in the artifact.
    pub observed_pairs: u32,
    /// Rootlight candidates with complete grades.
    pub rootlight_graded: u32,
    /// Bounded-file candidates with complete grades.
    pub bounded_file_graded: u32,
}

/// Resource totals reported separately from quality decisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowQualityEfficiency {
    /// Rootlight tool calls.
    pub rootlight_tool_calls: u64,
    /// Rootlight actual `o200k_base` tokens.
    pub rootlight_o200k_tokens: u64,
    /// Bounded-file tool calls.
    pub bounded_file_tool_calls: u64,
    /// Bounded-file actual `o200k_base` tokens.
    pub bounded_file_o200k_tokens: u64,
}

/// Complete candidate-bound, source-free all-workflow quality artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowQualityEvidence {
    /// Evidence schema.
    pub schema: String,
    /// Frozen grading protocol.
    pub protocol: WorkflowQualityProtocol,
    /// Digest of the complete grading protocol.
    pub protocol_sha256: String,
    /// Canonically ordered paired grades.
    pub pairs: Vec<WorkflowQualityPair>,
    /// Digest of all candidate bindings and paired grades.
    pub pairs_sha256: String,
    /// Canonically ordered per-workflow summaries.
    pub workflows: Vec<WorkflowQualitySummary>,
    /// Complete workflow and pair denominator.
    pub denominator: WorkflowQualityDenominator,
    /// Raw efficiency totals that do not affect quality.
    pub efficiency: WorkflowQualityEfficiency,
    /// True only when every workflow and seed pair satisfies the threshold.
    pub threshold_passed: bool,
}

impl WorkflowQualityEvidence {
    /// Validates protocol bindings, paired grades, denominators, aggregates,
    /// efficiency reconciliation, ordering, and the immutable threshold.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowQualityError`] when retained quality evidence can be
    /// detached from its trajectory, fixture, task, candidate, or score.
    pub fn validate(
        &self,
        trajectory: &TrajectoryEvidencePackage,
    ) -> Result<(), WorkflowQualityError> {
        if self.schema != WORKFLOW_QUALITY_SCHEMA_VERSION {
            return Err(WorkflowQualityError::UnsupportedSchema);
        }
        validate_protocol(&self.protocol, &trajectory.protocol)?;
        let protocol_sha256 =
            digest_json("rootlight.workflow-quality.protocol.v1", &self.protocol)?;
        if self.protocol_sha256 != protocol_sha256 {
            return Err(WorkflowQualityError::DigestMismatch);
        }
        if self.pairs_sha256 != digest_json("rootlight.workflow-quality.pairs.v1", &self.pairs)? {
            return Err(WorkflowQualityError::DigestMismatch);
        }
        let rebuilt = build_workflow_quality_evidence(
            trajectory,
            self.protocol.clone(),
            self.pairs
                .iter()
                .map(WorkflowQualityPairMeasurement::from)
                .collect(),
        )?;
        if *self != rebuilt {
            return Err(WorkflowQualityError::ReconciliationMismatch);
        }
        Ok(())
    }
}

impl From<&WorkflowQualityPair> for WorkflowQualityPairMeasurement {
    fn from(pair: &WorkflowQualityPair) -> Self {
        Self {
            workflow_id: pair.workflow_id.clone(),
            attempt_index: pair.attempt_index,
            rootlight: WorkflowQualityCandidateMeasurement::from(&pair.rootlight),
            bounded_file_exploration: WorkflowQualityCandidateMeasurement::from(
                &pair.bounded_file_exploration,
            ),
        }
    }
}

impl From<&WorkflowQualityCandidate> for WorkflowQualityCandidateMeasurement {
    fn from(candidate: &WorkflowQualityCandidate) -> Self {
        Self {
            condition: candidate.condition,
            attempt_id: candidate.attempt_id.clone(),
            candidate_sha256: candidate.candidate_sha256.clone(),
            dimensions: candidate.dimensions.clone(),
        }
    }
}

/// Creates a source-free grading protocol from held-out commitments.
///
/// The caller must invoke this before executing either candidate. Only prompt
/// and answer-key digests enter the returned protocol.
///
/// # Errors
///
/// Returns [`WorkflowQualityError`] when registrations do not cover every
/// workflow and attempt seed exactly once or contain malformed digests.
pub fn preregister_workflow_quality_protocol(
    trajectory: &TrajectoryProtocol,
    source_revision: impl Into<String>,
    registrations: Vec<WorkflowQualityTaskRegistration>,
) -> Result<WorkflowQualityProtocol, WorkflowQualityError> {
    trajectory
        .validate()
        .map_err(|_| WorkflowQualityError::InvalidTrajectory)?;
    let source_revision = source_revision.into();
    validate_revision(&source_revision)?;
    let trajectory_digests = trajectory
        .digests()
        .map_err(|_| WorkflowQualityError::InvalidTrajectory)?;
    let expected = trajectory
        .workflows
        .len()
        .checked_mul(trajectory.attempt_seeds.len())
        .ok_or(WorkflowQualityError::CounterOverflow)?;
    if registrations.len() != expected {
        return Err(WorkflowQualityError::IncompleteProtocol);
    }
    let mut by_key = BTreeMap::new();
    for registration in registrations {
        validate_label(&registration.workflow_id)?;
        validate_sha256(&registration.prompt_sha256)?;
        validate_sha256(&registration.answer_key_sha256)?;
        let key = (registration.workflow_id.clone(), registration.attempt_index);
        if by_key.insert(key, registration).is_some() {
            return Err(WorkflowQualityError::IncompleteProtocol);
        }
    }
    let mut tasks = Vec::with_capacity(expected);
    for workflow in &trajectory.workflows {
        for (attempt_index, seed) in trajectory.attempt_seeds.iter().copied().enumerate() {
            let attempt_index =
                u16::try_from(attempt_index).map_err(|_| WorkflowQualityError::CounterOverflow)?;
            let registration = by_key
                .remove(&(workflow.workflow_id.clone(), attempt_index))
                .ok_or(WorkflowQualityError::IncompleteProtocol)?;
            let task_sha256 = trajectory
                .task_digest(&workflow.workflow_id)
                .map_err(|_| WorkflowQualityError::InvalidTrajectory)?;
            tasks.push(WorkflowQualityTaskProtocol {
                workflow_id: workflow.workflow_id.clone(),
                family: workflow.family,
                task_sha256,
                fixture_sha256: trajectory.fixture_sha256.clone(),
                attempt_index,
                deterministic_seed: seed,
                prompt_sha256: registration.prompt_sha256,
                answer_key_sha256: registration.answer_key_sha256,
            });
        }
    }
    if !by_key.is_empty() {
        return Err(WorkflowQualityError::IncompleteProtocol);
    }
    let protocol = WorkflowQualityProtocol {
        experiment_id: "agent-workflow-quality-v1".to_owned(),
        source_revision,
        trajectory_protocol_sha256: trajectory_digests.protocol_sha256,
        trajectory_tasks_sha256: trajectory_digests.tasks_sha256,
        fixture_sha256: trajectory.fixture_sha256.clone(),
        rubric_sha256: fixed_rubric_sha256()?,
        maximum_quality_loss_centipoints: MAX_QUALITY_LOSS_CENTIPOINTS,
        tasks,
    };
    validate_protocol(&protocol, trajectory)?;
    Ok(protocol)
}

/// Builds and validates retained quality evidence from ephemeral grades.
///
/// Calls and `o200k_base` token totals are derived from trajectory attempts;
/// caller-supplied efficiency cannot influence the quality decision.
///
/// # Errors
///
/// Returns [`WorkflowQualityError`] when a candidate, score, attempt, or
/// denominator does not match the preregistered protocol and trajectory.
pub fn build_workflow_quality_evidence(
    trajectory: &TrajectoryEvidencePackage,
    protocol: WorkflowQualityProtocol,
    measurements: Vec<WorkflowQualityPairMeasurement>,
) -> Result<WorkflowQualityEvidence, WorkflowQualityError> {
    trajectory
        .validate()
        .map_err(|_| WorkflowQualityError::InvalidTrajectory)?;
    validate_protocol(&protocol, &trajectory.protocol)?;
    if measurements.len() != protocol.tasks.len() {
        return Err(WorkflowQualityError::IncompleteMeasurements);
    }
    let protocol_sha256 = digest_json("rootlight.workflow-quality.protocol.v1", &protocol)?;
    let attempts = trajectory
        .attempts
        .iter()
        .map(|attempt| {
            (
                (
                    attempt.workflow_id.as_str(),
                    attempt.attempt_index,
                    attempt.condition,
                ),
                attempt,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut measurements = measurements
        .into_iter()
        .map(|measurement| {
            (
                (measurement.workflow_id.clone(), measurement.attempt_index),
                measurement,
            )
        })
        .collect::<BTreeMap<_, _>>();
    if measurements.len() != protocol.tasks.len() {
        return Err(WorkflowQualityError::IncompleteMeasurements);
    }

    let mut pairs = Vec::with_capacity(protocol.tasks.len());
    let mut efficiency = WorkflowQualityEfficiency::default();
    for task in &protocol.tasks {
        let measurement = measurements
            .remove(&(task.workflow_id.clone(), task.attempt_index))
            .ok_or(WorkflowQualityError::IncompleteMeasurements)?;
        let rootlight_attempt = attempts
            .get(&(
                task.workflow_id.as_str(),
                task.attempt_index,
                TrajectoryCondition::Rootlight,
            ))
            .copied()
            .ok_or(WorkflowQualityError::AttemptMismatch)?;
        let bounded_attempt = attempts
            .get(&(
                task.workflow_id.as_str(),
                task.attempt_index,
                TrajectoryCondition::BoundedFileExploration,
            ))
            .copied()
            .ok_or(WorkflowQualityError::AttemptMismatch)?;
        let rootlight = candidate(
            &measurement.rootlight,
            TrajectoryCondition::Rootlight,
            rootlight_attempt,
        )?;
        let bounded = candidate(
            &measurement.bounded_file_exploration,
            TrajectoryCondition::BoundedFileExploration,
            bounded_attempt,
        )?;
        efficiency.rootlight_tool_calls = efficiency
            .rootlight_tool_calls
            .checked_add(u64::from(rootlight.tool_calls))
            .ok_or(WorkflowQualityError::CounterOverflow)?;
        efficiency.rootlight_o200k_tokens = efficiency
            .rootlight_o200k_tokens
            .checked_add(rootlight.o200k_tokens)
            .ok_or(WorkflowQualityError::CounterOverflow)?;
        efficiency.bounded_file_tool_calls = efficiency
            .bounded_file_tool_calls
            .checked_add(u64::from(bounded.tool_calls))
            .ok_or(WorkflowQualityError::CounterOverflow)?;
        efficiency.bounded_file_o200k_tokens = efficiency
            .bounded_file_o200k_tokens
            .checked_add(bounded.o200k_tokens)
            .ok_or(WorkflowQualityError::CounterOverflow)?;
        let loss = bounded
            .score_centipoints
            .saturating_sub(rootlight.score_centipoints);
        pairs.push(WorkflowQualityPair {
            workflow_id: task.workflow_id.clone(),
            family: task.family,
            task_sha256: task.task_sha256.clone(),
            fixture_sha256: task.fixture_sha256.clone(),
            attempt_index: task.attempt_index,
            deterministic_seed: task.deterministic_seed,
            prompt_sha256: task.prompt_sha256.clone(),
            answer_key_sha256: task.answer_key_sha256.clone(),
            rootlight,
            bounded_file_exploration: bounded,
            rootlight_quality_loss_centipoints: loss,
            within_threshold: loss <= MAX_QUALITY_LOSS_CENTIPOINTS,
        });
    }
    if !measurements.is_empty() {
        return Err(WorkflowQualityError::IncompleteMeasurements);
    }

    let workflows = summarize_workflows(&trajectory.protocol, &pairs)?;
    let pairs_sha256 = digest_json("rootlight.workflow-quality.pairs.v1", &pairs)?;
    let expected_pairs =
        u32::try_from(protocol.tasks.len()).map_err(|_| WorkflowQualityError::CounterOverflow)?;
    let denominator = WorkflowQualityDenominator {
        expected_workflows: u32::try_from(trajectory.protocol.workflows.len())
            .map_err(|_| WorkflowQualityError::CounterOverflow)?,
        observed_workflows: u32::try_from(workflows.len())
            .map_err(|_| WorkflowQualityError::CounterOverflow)?,
        expected_pairs,
        observed_pairs: u32::try_from(pairs.len())
            .map_err(|_| WorkflowQualityError::CounterOverflow)?,
        rootlight_graded: u32::try_from(pairs.len())
            .map_err(|_| WorkflowQualityError::CounterOverflow)?,
        bounded_file_graded: u32::try_from(pairs.len())
            .map_err(|_| WorkflowQualityError::CounterOverflow)?,
    };
    let threshold_passed = pairs.iter().all(|pair| pair.within_threshold)
        && workflows.iter().all(|workflow| workflow.within_threshold);
    Ok(WorkflowQualityEvidence {
        schema: WORKFLOW_QUALITY_SCHEMA_VERSION.to_owned(),
        protocol,
        protocol_sha256,
        pairs,
        pairs_sha256,
        workflows,
        denominator,
        efficiency,
        threshold_passed,
    })
}

/// Encodes a validated quality artifact as canonical compact JSON.
///
/// # Errors
///
/// Returns [`WorkflowQualityError`] when validation, serialization, or the
/// encoded-size bound fails.
pub fn encode_workflow_quality_evidence(
    evidence: &WorkflowQualityEvidence,
    trajectory: &TrajectoryEvidencePackage,
) -> Result<Vec<u8>, WorkflowQualityError> {
    evidence.validate(trajectory)?;
    let bytes = serde_json::to_vec(evidence).map_err(|_| WorkflowQualityError::InvalidEncoding)?;
    if bytes.len() > MAX_WORKFLOW_QUALITY_EVIDENCE_BYTES {
        return Err(WorkflowQualityError::EvidenceTooLarge);
    }
    Ok(bytes)
}

/// Decodes and validates a quality artifact against trajectory evidence.
///
/// # Errors
///
/// Returns [`WorkflowQualityError`] for malformed, oversized, detached, or
/// internally inconsistent evidence.
pub fn decode_workflow_quality_evidence(
    bytes: &[u8],
    trajectory: &TrajectoryEvidencePackage,
) -> Result<WorkflowQualityEvidence, WorkflowQualityError> {
    if bytes.len() > MAX_WORKFLOW_QUALITY_EVIDENCE_BYTES {
        return Err(WorkflowQualityError::EvidenceTooLarge);
    }
    let evidence: WorkflowQualityEvidence =
        serde_json::from_slice(bytes).map_err(|_| WorkflowQualityError::InvalidEncoding)?;
    evidence.validate(trajectory)?;
    Ok(evidence)
}

fn validate_protocol(
    protocol: &WorkflowQualityProtocol,
    trajectory: &TrajectoryProtocol,
) -> Result<(), WorkflowQualityError> {
    trajectory
        .validate()
        .map_err(|_| WorkflowQualityError::InvalidTrajectory)?;
    validate_label(&protocol.experiment_id)?;
    validate_revision(&protocol.source_revision)?;
    for digest in [
        &protocol.trajectory_protocol_sha256,
        &protocol.trajectory_tasks_sha256,
        &protocol.fixture_sha256,
        &protocol.rubric_sha256,
    ] {
        validate_sha256(digest)?;
    }
    let digests = trajectory
        .digests()
        .map_err(|_| WorkflowQualityError::InvalidTrajectory)?;
    if protocol.experiment_id != "agent-workflow-quality-v1"
        || protocol.trajectory_protocol_sha256 != digests.protocol_sha256
        || protocol.trajectory_tasks_sha256 != digests.tasks_sha256
        || protocol.fixture_sha256 != trajectory.fixture_sha256
        || protocol.rubric_sha256 != fixed_rubric_sha256()?
        || protocol.maximum_quality_loss_centipoints != MAX_QUALITY_LOSS_CENTIPOINTS
        || trajectory.workflows.len() != EXPECTED_WORKFLOW_COUNT
    {
        return Err(WorkflowQualityError::ProtocolMismatch);
    }
    let expected = trajectory
        .workflows
        .len()
        .checked_mul(trajectory.attempt_seeds.len())
        .ok_or(WorkflowQualityError::CounterOverflow)?;
    if protocol.tasks.len() != expected {
        return Err(WorkflowQualityError::IncompleteProtocol);
    }
    let mut index = 0_usize;
    for workflow in &trajectory.workflows {
        for (attempt_index, seed) in trajectory.attempt_seeds.iter().copied().enumerate() {
            let task = protocol
                .tasks
                .get(index)
                .ok_or(WorkflowQualityError::IncompleteProtocol)?;
            let attempt_index =
                u16::try_from(attempt_index).map_err(|_| WorkflowQualityError::CounterOverflow)?;
            if task.workflow_id != workflow.workflow_id
                || task.family != workflow.family
                || task.task_sha256
                    != trajectory
                        .task_digest(&workflow.workflow_id)
                        .map_err(|_| WorkflowQualityError::InvalidTrajectory)?
                || task.fixture_sha256 != trajectory.fixture_sha256
                || task.attempt_index != attempt_index
                || task.deterministic_seed != seed
            {
                return Err(WorkflowQualityError::ProtocolMismatch);
            }
            validate_sha256(&task.prompt_sha256)?;
            validate_sha256(&task.answer_key_sha256)?;
            index = index
                .checked_add(1)
                .ok_or(WorkflowQualityError::CounterOverflow)?;
        }
    }
    Ok(())
}

fn candidate(
    measurement: &WorkflowQualityCandidateMeasurement,
    condition: TrajectoryCondition,
    attempt: &crate::TrajectoryAttemptRecord,
) -> Result<WorkflowQualityCandidate, WorkflowQualityError> {
    validate_sha256(&measurement.candidate_sha256)?;
    validate_dimensions(&measurement.dimensions)?;
    if measurement.condition != condition
        || measurement.attempt_id != attempt.attempt_id
        || attempt.condition != condition
    {
        return Err(WorkflowQualityError::AttemptMismatch);
    }
    if !matches!(attempt.outcome, TrajectoryAttemptOutcome::Succeeded) {
        return Err(WorkflowQualityError::CandidateDidNotSucceed);
    }
    let tool_calls =
        u32::try_from(attempt.calls.len()).map_err(|_| WorkflowQualityError::CounterOverflow)?;
    let o200k_tokens = attempt.calls.iter().try_fold(0_u64, |total, call| {
        let tokens = call
            .accounting
            .total
            .actual_tokens
            .ok_or(WorkflowQualityError::MissingActualTokens)?;
        total
            .checked_add(tokens)
            .ok_or(WorkflowQualityError::CounterOverflow)
    })?;
    let score_centipoints = measurement
        .dimensions
        .iter()
        .try_fold(0_u16, |total, dimension| {
            total
                .checked_add(dimension.earned_centipoints)
                .ok_or(WorkflowQualityError::CounterOverflow)
        })?;
    Ok(WorkflowQualityCandidate {
        condition,
        attempt_id: measurement.attempt_id.clone(),
        candidate_sha256: measurement.candidate_sha256.clone(),
        score_centipoints,
        dimensions: measurement.dimensions.clone(),
        tool_calls,
        o200k_tokens,
    })
}

fn validate_dimensions(
    dimensions: &[WorkflowQualityDimensionScore],
) -> Result<(), WorkflowQualityError> {
    if dimensions.len() != WorkflowQualityDimension::ALL.len() {
        return Err(WorkflowQualityError::InvalidScore);
    }
    for (score, (dimension, maximum)) in dimensions.iter().zip(WorkflowQualityDimension::ALL) {
        if score.dimension != dimension
            || score.maximum_centipoints != maximum
            || score.earned_centipoints > maximum
        {
            return Err(WorkflowQualityError::InvalidScore);
        }
    }
    Ok(())
}

fn summarize_workflows(
    trajectory: &TrajectoryProtocol,
    pairs: &[WorkflowQualityPair],
) -> Result<Vec<WorkflowQualitySummary>, WorkflowQualityError> {
    let expected_pairs = u32::try_from(trajectory.attempt_seeds.len())
        .map_err(|_| WorkflowQualityError::CounterOverflow)?;
    let mut summaries = Vec::with_capacity(trajectory.workflows.len());
    for workflow in &trajectory.workflows {
        let workflow_pairs = pairs
            .iter()
            .filter(|pair| pair.workflow_id == workflow.workflow_id)
            .collect::<Vec<_>>();
        if workflow_pairs.len() != trajectory.attempt_seeds.len() {
            return Err(WorkflowQualityError::IncompleteMeasurements);
        }
        let rootlight_sum = workflow_pairs.iter().try_fold(0_u64, |total, pair| {
            total
                .checked_add(u64::from(pair.rootlight.score_centipoints))
                .ok_or(WorkflowQualityError::CounterOverflow)
        })?;
        let bounded_sum = workflow_pairs.iter().try_fold(0_u64, |total, pair| {
            total
                .checked_add(u64::from(pair.bounded_file_exploration.score_centipoints))
                .ok_or(WorkflowQualityError::CounterOverflow)
        })?;
        let divisor = u64::try_from(workflow_pairs.len())
            .map_err(|_| WorkflowQualityError::CounterOverflow)?;
        let rootlight_mean = u16::try_from(rootlight_sum / divisor)
            .map_err(|_| WorkflowQualityError::CounterOverflow)?;
        let bounded_mean = u16::try_from(bounded_sum / divisor)
            .map_err(|_| WorkflowQualityError::CounterOverflow)?;
        let loss = bounded_mean.saturating_sub(rootlight_mean);
        let maximum_pair_loss = workflow_pairs
            .iter()
            .map(|pair| pair.rootlight_quality_loss_centipoints)
            .max()
            .unwrap_or(0);
        summaries.push(WorkflowQualitySummary {
            workflow_id: workflow.workflow_id.clone(),
            expected_pairs,
            observed_pairs: u32::try_from(workflow_pairs.len())
                .map_err(|_| WorkflowQualityError::CounterOverflow)?,
            rootlight_mean_quality_centipoints: rootlight_mean,
            bounded_file_mean_quality_centipoints: bounded_mean,
            rootlight_quality_loss_centipoints: loss,
            maximum_pair_loss_centipoints: maximum_pair_loss,
            within_threshold: loss <= MAX_QUALITY_LOSS_CENTIPOINTS
                && maximum_pair_loss <= MAX_QUALITY_LOSS_CENTIPOINTS,
        });
    }
    Ok(summaries)
}

#[derive(Serialize)]
struct FixedRubric {
    dimensions: Vec<WorkflowQualityDimensionScore>,
    maximum_quality_loss_centipoints: u16,
    efficiency_affects_quality: bool,
}

fn fixed_rubric_sha256() -> Result<String, WorkflowQualityError> {
    digest_json(
        "rootlight.workflow-quality.rubric.v1",
        &FixedRubric {
            dimensions: WorkflowQualityDimension::ALL
                .into_iter()
                .map(
                    |(dimension, maximum_centipoints)| WorkflowQualityDimensionScore {
                        dimension,
                        earned_centipoints: maximum_centipoints,
                        maximum_centipoints,
                    },
                )
                .collect(),
            maximum_quality_loss_centipoints: MAX_QUALITY_LOSS_CENTIPOINTS,
            efficiency_affects_quality: false,
        },
    )
}

fn validate_label(value: &str) -> Result<(), WorkflowQualityError> {
    if value.is_empty()
        || value.len() > MAX_LABEL_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(WorkflowQualityError::InvalidLabel);
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), WorkflowQualityError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(WorkflowQualityError::InvalidDigest);
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), WorkflowQualityError> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(WorkflowQualityError::InvalidRevision);
    }
    Ok(())
}

fn digest_json(domain: &str, value: &impl Serialize) -> Result<String, WorkflowQualityError> {
    let bytes = serde_json::to_vec(value).map_err(|_| WorkflowQualityError::InvalidEncoding)?;
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    Ok(encoded)
}

/// Quality protocol, grading, or evidence validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkflowQualityError {
    /// The evidence schema is unsupported.
    #[error("unsupported workflow quality schema")]
    UnsupportedSchema,
    /// The linked trajectory evidence is invalid.
    #[error("invalid linked trajectory evidence")]
    InvalidTrajectory,
    /// A label is malformed or outside its bound.
    #[error("invalid workflow quality label")]
    InvalidLabel,
    /// A SHA-256 digest is malformed.
    #[error("invalid workflow quality digest")]
    InvalidDigest,
    /// A source revision is not canonical hexadecimal.
    #[error("invalid workflow quality source revision")]
    InvalidRevision,
    /// The preregistration omits or duplicates a task instance.
    #[error("incomplete workflow quality protocol")]
    IncompleteProtocol,
    /// The quality protocol does not match the trajectory protocol.
    #[error("workflow quality protocol mismatch")]
    ProtocolMismatch,
    /// Measurements omit or duplicate a preregistered task instance.
    #[error("incomplete workflow quality measurements")]
    IncompleteMeasurements,
    /// A candidate is not bound to the expected trajectory attempt.
    #[error("workflow quality attempt mismatch")]
    AttemptMismatch,
    /// A candidate trajectory did not reach terminal success.
    #[error("workflow quality candidate did not succeed")]
    CandidateDidNotSucceed,
    /// A candidate score vector is incomplete or exceeds its weight.
    #[error("invalid workflow quality score")]
    InvalidScore,
    /// Actual tokenizer accounting is absent from a linked attempt.
    #[error("workflow quality attempt is missing actual tokens")]
    MissingActualTokens,
    /// A retained digest differs from recomputed content.
    #[error("workflow quality digest mismatch")]
    DigestMismatch,
    /// Retained summaries differ from their paired measurements.
    #[error("workflow quality reconciliation mismatch")]
    ReconciliationMismatch,
    /// A checked counter overflowed.
    #[error("workflow quality counter overflow")]
    CounterOverflow,
    /// JSON encoding or decoding failed.
    #[error("invalid workflow quality encoding")]
    InvalidEncoding,
    /// The encoded artifact exceeds its public limit.
    #[error("workflow quality evidence exceeds size limit")]
    EvidenceTooLarge,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{
        BoundedFileExplorationAdapter, O200kTrajectoryTokenizer, RawTrajectoryAttempt,
        RawTrajectoryCall, TrajectoryAdapter, TrajectoryClaimSignals, TrajectoryExecutionBoundary,
        TrajectoryExecutionInput, TrajectoryExposureProfile, TrajectoryOperationStatus,
        TrajectoryToolIdentity, UnavailableTrajectoryAdapter, preregistered_trajectory_protocol,
        run_trajectory_suite, sha256_hex,
    };

    struct SuccessfulRootlight;

    impl TrajectoryAdapter for SuccessfulRootlight {
        fn condition(&self) -> TrajectoryCondition {
            TrajectoryCondition::Rootlight
        }

        fn execution_boundary(&self) -> TrajectoryExecutionBoundary {
            TrajectoryExecutionBoundary::DaemonMcpProcess
        }

        fn execute(&mut self, input: TrajectoryExecutionInput<'_>) -> RawTrajectoryAttempt {
            RawTrajectoryAttempt {
                outcome: TrajectoryAttemptOutcome::Succeeded,
                calls: vec![RawTrajectoryCall {
                    operation_id: "task".to_owned(),
                    tool: TrajectoryToolIdentity {
                        tool_id: input.workflow.rootlight_tools[0].clone(),
                        tool_version: "test-v1".to_owned(),
                    },
                    exposure_profile: TrajectoryExposureProfile::Analysis,
                    operation_status: TrajectoryOperationStatus::Succeeded,
                    retry_ordinal: 0,
                    request_frame: b"{\"task\":\"fixture\"}".to_vec(),
                    response_frame: b"{\"status\":\"succeeded\"}".to_vec(),
                    source_frame: Vec::new(),
                    elapsed_ns: 1,
                    result_items: 1,
                    truncated: false,
                    continuation_available: false,
                    claim_signals: TrajectoryClaimSignals::default(),
                }],
            }
        }
    }

    #[test]
    fn evidence_round_trips_and_mutations_fail_closed() {
        let temporary = tempfile::tempdir().expect("temporary quality fixture is available");
        fs::write(
            temporary.path().join("lib.rs"),
            "pub fn budget_entry() { budget_helper(); }\npub fn budget_helper() {}\n",
        )
        .expect("quality fixture is written");
        let fixture_sha256 = sha256_hex(b"quality-fixture-v1");
        let protocol =
            preregistered_trajectory_protocol(fixture_sha256).expect("trajectory protocol builds");
        let registrations = protocol
            .workflows
            .iter()
            .flat_map(|workflow| {
                protocol.attempt_seeds.iter().copied().enumerate().map(
                    move |(attempt_index, seed)| WorkflowQualityTaskRegistration {
                        workflow_id: workflow.workflow_id.clone(),
                        attempt_index: u16::try_from(attempt_index)
                            .expect("attempt index fits u16"),
                        prompt_sha256: sha256_hex(
                            format!("{}-{seed}", workflow.workflow_id).as_bytes(),
                        ),
                        answer_key_sha256: sha256_hex(
                            format!("answer-{}-{seed}", workflow.workflow_id).as_bytes(),
                        ),
                    },
                )
            })
            .collect();
        let quality_protocol =
            preregister_workflow_quality_protocol(&protocol, "a".repeat(40), registrations)
                .expect("quality protocol builds");
        let mut rootlight = SuccessfulRootlight;
        let mut unavailable = UnavailableTrajectoryAdapter::new(
            TrajectoryCondition::CodebaseMemory,
            "codebase-memory",
            "executable-not-available",
        )
        .expect("optional adapter absence is valid");
        let mut bounded = BoundedFileExplorationAdapter::new(temporary.path());
        let tokenizer = O200kTrajectoryTokenizer::new().expect("tokenizer initializes");
        let trajectory = run_trajectory_suite(
            protocol,
            &mut rootlight,
            &mut unavailable,
            &mut bounded,
            &tokenizer,
        )
        .expect("trajectory evidence builds");
        let measurements = quality_protocol
            .tasks
            .iter()
            .map(|task| WorkflowQualityPairMeasurement {
                workflow_id: task.workflow_id.clone(),
                attempt_index: task.attempt_index,
                rootlight: measurement(
                    &trajectory,
                    task,
                    TrajectoryCondition::Rootlight,
                    "rootlight",
                ),
                bounded_file_exploration: measurement(
                    &trajectory,
                    task,
                    TrajectoryCondition::BoundedFileExploration,
                    "bounded",
                ),
            })
            .collect();
        let evidence = build_workflow_quality_evidence(&trajectory, quality_protocol, measurements)
            .expect("quality evidence builds");
        let encoded = encode_workflow_quality_evidence(&evidence, &trajectory)
            .expect("quality evidence encodes");
        assert_eq!(
            decode_workflow_quality_evidence(&encoded, &trajectory)
                .expect("quality evidence decodes"),
            evidence
        );

        let mut threshold = evidence.clone();
        threshold.protocol.maximum_quality_loss_centipoints = 201;
        assert!(matches!(
            threshold.validate(&trajectory),
            Err(WorkflowQualityError::ProtocolMismatch)
        ));
        let mut candidate = evidence.clone();
        candidate.pairs[0].rootlight.candidate_sha256 = "b".repeat(64);
        assert!(matches!(
            candidate.validate(&trajectory),
            Err(WorkflowQualityError::DigestMismatch)
        ));
        let mut dimension = evidence.clone();
        dimension.pairs[0].rootlight.dimensions[0].earned_centipoints = 2_501;
        dimension.pairs_sha256 =
            digest_json("rootlight.workflow-quality.pairs.v1", &dimension.pairs)
                .expect("mutated pair digest recomputes");
        assert!(matches!(
            dimension.validate(&trajectory),
            Err(WorkflowQualityError::InvalidScore)
        ));
    }

    fn measurement(
        trajectory: &TrajectoryEvidencePackage,
        task: &WorkflowQualityTaskProtocol,
        condition: TrajectoryCondition,
        candidate: &str,
    ) -> WorkflowQualityCandidateMeasurement {
        let attempt = trajectory
            .attempts
            .iter()
            .find(|attempt| {
                attempt.workflow_id == task.workflow_id
                    && attempt.attempt_index == task.attempt_index
                    && attempt.condition == condition
            })
            .expect("candidate attempt exists");
        WorkflowQualityCandidateMeasurement {
            condition,
            attempt_id: attempt.attempt_id.clone(),
            candidate_sha256: sha256_hex(
                format!("{candidate}-{}-{}", task.workflow_id, task.attempt_index).as_bytes(),
            ),
            dimensions: WorkflowQualityDimension::ALL
                .into_iter()
                .map(
                    |(dimension, maximum_centipoints)| WorkflowQualityDimensionScore {
                        dimension,
                        earned_centipoints: maximum_centipoints,
                        maximum_centipoints,
                    },
                )
                .collect(),
        }
    }
}
