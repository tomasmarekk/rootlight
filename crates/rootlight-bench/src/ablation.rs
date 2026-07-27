//! Blinded context-pack ablation grading over source-free trajectory evidence.
//!
//! Condition identities stay in a separate restricted pairing map until both
//! deterministic automated graders have finalized their raw grades.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    TrajectoryAttemptOutcome, TrajectoryCondition, TrajectoryEvidencePackage,
    TrajectorySharedBounds, TrajectoryStoppingPolicy, sha256_hex,
};

/// Schema for preregistered context-pack ablation evidence.
pub const ABLATION_SCHEMA_VERSION: &str = "rootlight.context-pack-ablation/1";
/// Maximum accepted quality loss in hundredths of one point.
pub const MAX_QUALITY_LOSS_CENTIPOINTS: u16 = 200;
/// Maximum score on the zero-to-one-hundred quality scale.
pub const MAX_QUALITY_SCORE_CENTIPOINTS: u16 = 10_000;
/// Fixed number of paired bootstrap replicates.
pub const PAIRED_BOOTSTRAP_REPLICATES: usize = 1_024;
/// Maximum checks accepted for one rubric dimension.
pub const MAX_CHECKS_PER_DIMENSION: usize = 16;
/// Maximum encoded public-plus-restricted evidence bytes.
pub const MAX_ABLATION_EVIDENCE_BYTES: usize = 8 * 1024 * 1024;

const CONTEXT_WORKFLOW_ID: &str = "bug-fix-context";
const AUTOMATED_GRADER_A_ID: &str = "automated-check-mean-v1";
const AUTOMATED_GRADER_B_ID: &str = "automated-strict-conjunction-v1";
const AUTOMATED_ADJUDICATOR_ID: &str = "automated-conservative-adjudicator-v1";

/// Secret input used only to derive opaque candidate and ordering identities.
#[derive(Clone, PartialEq, Eq)]
pub struct AblationBlindingKey([u8; 32]);

impl AblationBlindingKey {
    /// Creates a blinding key from exactly 256 bits of caller-held material.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the public commitment stored in the preregistered protocol.
    #[must_use]
    pub fn commitment_sha256(&self) -> String {
        sha256_hex(&self.0)
    }
}

impl fmt::Debug for AblationBlindingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AblationBlindingKey(***)")
    }
}

/// Closed ablation variants frozen before grading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AblationVariant {
    /// Rootlight workflow using `context.pack`.
    ContextPack,
    /// Equivalent Rootlight workflow using only direct tools.
    DirectSequence,
    /// Optional reproduced Codebase-Memory baseline.
    CodebaseMemory,
    /// Bounded regular-file exploration baseline.
    BoundedFileExploration,
}

impl AblationVariant {
    const ALL: [Self; 4] = [
        Self::ContextPack,
        Self::DirectSequence,
        Self::CodebaseMemory,
        Self::BoundedFileExploration,
    ];
}

/// One fixed selector defining how a variant must be represented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AblationVariantProtocol {
    /// Closed variant identity.
    pub variant: AblationVariant,
    /// Workflow identity that must remain equivalent across variants.
    pub workflow_id: String,
    /// Required trajectory condition, when the variant is representable there.
    pub condition: TrajectoryCondition,
    /// Canonically ordered tools proving the intended execution mode.
    pub required_tools: Vec<String>,
    /// Whether an absent executable remains an observed optional baseline.
    pub optional: bool,
}

/// Fixed seven-dimension grading rubric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RubricDimension {
    /// Factual and semantic correctness.
    Correctness,
    /// Coverage of required task evidence.
    Completeness,
    /// Support for claims from retained evidence.
    EvidenceSupport,
    /// Explicit handling of uncertainty and partial results.
    UncertaintyHandling,
    /// Usefulness of the answer for the requested next action.
    Actionability,
    /// Relevance of selected source evidence.
    SourceRelevance,
    /// Adherence to the task's explicit requirements.
    TaskAdherence,
}

impl RubricDimension {
    const ALL: [Self; 7] = [
        Self::Correctness,
        Self::Completeness,
        Self::EvidenceSupport,
        Self::UncertaintyHandling,
        Self::Actionability,
        Self::SourceRelevance,
        Self::TaskAdherence,
    ];

    const fn weight(self) -> u16 {
        match self {
            Self::Correctness => 25,
            Self::Completeness | Self::EvidenceSupport => 20,
            Self::UncertaintyHandling | Self::Actionability | Self::SourceRelevance => 10,
            Self::TaskAdherence => 5,
        }
    }
}

/// Frozen rubric definition and automated grading identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AblationRubricProtocol {
    /// Canonically ordered dimensions and integer percentage weights.
    pub dimensions: Vec<RubricDimensionWeight>,
    /// First independent automated grader identity.
    pub automated_grader_a_id: String,
    /// Second independent automated grader identity.
    pub automated_grader_b_id: String,
    /// Bounded automated adjudicator identity.
    pub automated_adjudicator_id: String,
    /// Maximum automated adjudications retained per candidate.
    pub max_adjudications_per_candidate: u8,
    /// Closed unsupported-claim categories graders must assess.
    pub unsupported_claim_categories: Vec<UnsupportedClaimCategory>,
}

/// One rubric dimension and its preregistered integer weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RubricDimensionWeight {
    /// Rubric dimension.
    pub dimension: RubricDimension,
    /// Percentage weight; all seven entries sum to one hundred.
    pub weight_percent: u16,
}

/// Complete paired ablation and grading preregistration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AblationProtocol {
    /// Protocol schema.
    pub schema: String,
    /// Stable experiment identity.
    pub experiment_id: String,
    /// Exact source revision supplied by the evidence orchestrator.
    pub source_revision: String,
    /// Digest of the immutable trajectory protocol.
    pub trajectory_protocol_sha256: String,
    /// Digest of the exact task definition subset.
    pub task_subset_sha256: String,
    /// Digest of fixture generations and evidence universe.
    pub evidence_universe_sha256: String,
    /// Digest of model, tokenizer, and runner configuration.
    pub model_configuration_sha256: String,
    /// Public commitment to the caller-held blinding key.
    pub blinding_key_sha256: String,
    /// Deterministic seed for candidate ordering and bootstrap draws.
    pub randomization_seed: u64,
    /// Shared total resource bounds copied from the trajectory protocol.
    pub shared_bounds: TrajectorySharedBounds,
    /// Shared stopping policy copied from the trajectory protocol.
    pub stopping: TrajectoryStoppingPolicy,
    /// Shared retry-policy digest copied from the trajectory protocol.
    pub retry_policy_sha256: String,
    /// Fixed attempt seeds defining paired task instances.
    pub attempt_seeds: Vec<u64>,
    /// Canonically ordered ablation variants.
    pub variants: Vec<AblationVariantProtocol>,
    /// Fixed blinded rubric and automated grading rules.
    pub rubric: AblationRubricProtocol,
    /// Immutable maximum quality loss in hundredths of one point.
    pub max_quality_loss_centipoints: u16,
}

impl AblationProtocol {
    /// Validates fixed pairing, grading, blinding, and threshold rules.
    ///
    /// # Errors
    ///
    /// Returns [`AblationError`] when identifiers, digests, ordering, weights,
    /// variants, budgets, or the immutable threshold are invalid.
    pub fn validate(&self) -> Result<(), AblationError> {
        if self.schema != ABLATION_SCHEMA_VERSION {
            return Err(AblationError::UnsupportedSchema);
        }
        validate_label(&self.experiment_id)?;
        validate_revision(&self.source_revision)?;
        for digest in [
            &self.trajectory_protocol_sha256,
            &self.task_subset_sha256,
            &self.evidence_universe_sha256,
            &self.model_configuration_sha256,
            &self.blinding_key_sha256,
            &self.retry_policy_sha256,
        ] {
            validate_digest(digest)?;
        }
        if self.max_quality_loss_centipoints != MAX_QUALITY_LOSS_CENTIPOINTS
            || self.randomization_seed == 0
            || self.shared_bounds.tool_calls == 0
            || self.shared_bounds.elapsed_ns == 0
            || self.shared_bounds.result_items == 0
            || self.shared_bounds.source_bytes == 0
            || self.shared_bounds.total_tokens == 0
            || self.attempt_seeds.is_empty()
            || self.attempt_seeds.windows(2).any(|pair| pair[0] >= pair[1])
            || self.variants.len() != AblationVariant::ALL.len()
        {
            return Err(AblationError::InvalidProtocol);
        }
        for (variant, expected) in self.variants.iter().zip(AblationVariant::ALL) {
            if variant.variant != expected || variant.workflow_id != CONTEXT_WORKFLOW_ID {
                return Err(AblationError::InvalidProtocol);
            }
            validate_label(&variant.workflow_id)?;
            validate_sorted_labels(&variant.required_tools)?;
            if variant.required_tools.is_empty() {
                return Err(AblationError::InvalidProtocol);
            }
        }
        let context = &self.variants[0];
        let direct = &self.variants[1];
        let codebase = &self.variants[2];
        let files = &self.variants[3];
        if context.condition != TrajectoryCondition::Rootlight
            || context.optional
            || context.required_tools != ["code.locate", "context.pack"]
            || direct.condition != TrajectoryCondition::Rootlight
            || direct.optional
            || direct.required_tools.contains(&"context.pack".to_owned())
            || codebase.condition != TrajectoryCondition::CodebaseMemory
            || !codebase.optional
            || files.condition != TrajectoryCondition::BoundedFileExploration
            || files.optional
        {
            return Err(AblationError::InvalidProtocol);
        }
        self.rubric.validate()
    }

    fn digest(&self) -> Result<String, AblationError> {
        digest_json("rootlight.ablation.protocol.v1", self)
    }
}

impl AblationRubricProtocol {
    fn validate(&self) -> Result<(), AblationError> {
        if self.automated_grader_a_id != AUTOMATED_GRADER_A_ID
            || self.automated_grader_b_id != AUTOMATED_GRADER_B_ID
            || self.automated_adjudicator_id != AUTOMATED_ADJUDICATOR_ID
            || self.max_adjudications_per_candidate != 7
            || self.dimensions.len() != RubricDimension::ALL.len()
            || self.unsupported_claim_categories != UnsupportedClaimCategory::ALL
        {
            return Err(AblationError::InvalidProtocol);
        }
        for identifier in [
            &self.automated_grader_a_id,
            &self.automated_grader_b_id,
            &self.automated_adjudicator_id,
        ] {
            validate_label(identifier)?;
        }
        let mut total = 0_u16;
        for (entry, expected) in self.dimensions.iter().zip(RubricDimension::ALL) {
            if entry.dimension != expected || entry.weight_percent != expected.weight() {
                return Err(AblationError::InvalidProtocol);
            }
            total = total
                .checked_add(entry.weight_percent)
                .ok_or(AblationError::CounterOverflow)?;
        }
        if total != 100 {
            return Err(AblationError::InvalidProtocol);
        }
        Ok(())
    }
}

/// Creates the immutable ablation protocol from one validated trajectory package.
///
/// # Errors
///
/// Returns [`AblationError`] when the source package is invalid or required
/// task/configuration bindings cannot be constructed.
pub fn preregister_context_pack_ablation(
    package: &TrajectoryEvidencePackage,
    blinding_key: &AblationBlindingKey,
    source_revision: &str,
) -> Result<AblationProtocol, AblationError> {
    package.validate()?;
    validate_revision(source_revision)?;
    let task = package
        .protocol
        .workflows
        .iter()
        .find(|workflow| workflow.workflow_id == CONTEXT_WORKFLOW_ID)
        .ok_or(AblationError::MissingContextTask)?;
    let protocol = AblationProtocol {
        schema: ABLATION_SCHEMA_VERSION.to_owned(),
        experiment_id: "context-pack-ablation-v1".to_owned(),
        source_revision: source_revision.to_owned(),
        trajectory_protocol_sha256: package.digests.protocol_sha256.clone(),
        task_subset_sha256: digest_json("rootlight.ablation.task-subset.v1", task)?,
        evidence_universe_sha256: digest_json(
            "rootlight.ablation.evidence-universe.v1",
            &(
                package.digests.fixture_sha256.as_str(),
                package.protocol.fixture_id.as_str(),
            ),
        )?,
        model_configuration_sha256: digest_json(
            "rootlight.ablation.model-configuration.v1",
            &(
                package.digests.configuration_sha256.as_str(),
                package.digests.runner_sha256.as_str(),
            ),
        )?,
        blinding_key_sha256: blinding_key.commitment_sha256(),
        randomization_seed: 0x524f_4f54_4c49_4748,
        shared_bounds: package.protocol.bounds,
        stopping: package.protocol.stopping,
        retry_policy_sha256: digest_json(
            "rootlight.ablation.retry-policy.v1",
            &package.protocol.retry,
        )?,
        attempt_seeds: package.protocol.attempt_seeds.clone(),
        variants: vec![
            AblationVariantProtocol {
                variant: AblationVariant::ContextPack,
                workflow_id: CONTEXT_WORKFLOW_ID.to_owned(),
                condition: TrajectoryCondition::Rootlight,
                required_tools: vec!["code.locate".to_owned(), "context.pack".to_owned()],
                optional: false,
            },
            AblationVariantProtocol {
                variant: AblationVariant::DirectSequence,
                workflow_id: CONTEXT_WORKFLOW_ID.to_owned(),
                condition: TrajectoryCondition::Rootlight,
                required_tools: vec![
                    "code.locate".to_owned(),
                    "source.read".to_owned(),
                    "symbol.explain".to_owned(),
                    "symbol.relationships".to_owned(),
                ],
                optional: false,
            },
            AblationVariantProtocol {
                variant: AblationVariant::CodebaseMemory,
                workflow_id: CONTEXT_WORKFLOW_ID.to_owned(),
                condition: TrajectoryCondition::CodebaseMemory,
                required_tools: vec!["codebase_memory_process_v1".to_owned()],
                optional: true,
            },
            AblationVariantProtocol {
                variant: AblationVariant::BoundedFileExploration,
                workflow_id: CONTEXT_WORKFLOW_ID.to_owned(),
                condition: TrajectoryCondition::BoundedFileExploration,
                required_tools: vec!["bounded_file.explore".to_owned()],
                optional: false,
            },
        ],
        rubric: AblationRubricProtocol {
            dimensions: RubricDimension::ALL
                .into_iter()
                .map(|dimension| RubricDimensionWeight {
                    dimension,
                    weight_percent: dimension.weight(),
                })
                .collect(),
            automated_grader_a_id: AUTOMATED_GRADER_A_ID.to_owned(),
            automated_grader_b_id: AUTOMATED_GRADER_B_ID.to_owned(),
            automated_adjudicator_id: AUTOMATED_ADJUDICATOR_ID.to_owned(),
            max_adjudications_per_candidate: 7,
            unsupported_claim_categories: UnsupportedClaimCategory::ALL.to_vec(),
        },
        max_quality_loss_centipoints: MAX_QUALITY_LOSS_CENTIPOINTS,
    };
    protocol.validate()?;
    Ok(protocol)
}

/// Closed attempt state exposed to graders without a condition identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlindedRunOutcome {
    /// The trajectory reached its declared terminal success.
    Succeeded,
    /// The trajectory failed.
    Failed,
    /// The trajectory timed out.
    TimedOut,
    /// The trajectory was cancelled.
    Cancelled,
    /// The adapter reported an unsupported capability.
    Unsupported,
    /// An optional executable was not available.
    NotAvailable,
    /// A preregistered exclusion was retained.
    Excluded,
}

impl From<&TrajectoryAttemptOutcome> for BlindedRunOutcome {
    fn from(outcome: &TrajectoryAttemptOutcome) -> Self {
        match outcome {
            TrajectoryAttemptOutcome::Succeeded => Self::Succeeded,
            TrajectoryAttemptOutcome::Failed { .. } => Self::Failed,
            TrajectoryAttemptOutcome::TimedOut { .. } => Self::TimedOut,
            TrajectoryAttemptOutcome::Cancelled { .. } => Self::Cancelled,
            TrajectoryAttemptOutcome::Unsupported { .. } => Self::Unsupported,
            TrajectoryAttemptOutcome::NotAvailable { .. } => Self::NotAvailable,
            TrajectoryAttemptOutcome::Excluded { .. } => Self::Excluded,
        }
    }
}

/// Source-free resource and unsupported-claim counters for one candidate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlindedCandidateMetrics {
    /// Retained tool calls.
    pub calls: u64,
    /// Actual request-plus-response tokens.
    pub tokens: u64,
    /// Actual source-attributed tokens.
    pub source_tokens: u64,
    /// Monotonic elapsed nanoseconds.
    pub elapsed_ns: u64,
    /// Unsupported claims observed by the trajectory producer.
    pub unsupported_claims: u32,
}

/// One randomized candidate visible to automated graders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlindedAblationCandidate {
    /// Opaque identity containing no condition label or attempt identifier.
    pub blind_id: String,
    /// Opaque paired-input identity.
    pub pair_id: String,
    /// Digest of the exact shared task definition.
    pub task_sha256: String,
    /// Closed attempt outcome.
    pub outcome: BlindedRunOutcome,
    /// Source-free counters; tool identities and raw frames are absent.
    pub metrics: BlindedCandidateMetrics,
    /// Digest binding every visible candidate field.
    pub candidate_sha256: String,
}

/// Restricted identity mapping kept separate from blinded grading records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestrictedPairingMap {
    /// Ablation protocol digest.
    pub protocol_sha256: String,
    /// Canonically ordered pair definitions.
    pub pairs: Vec<RestrictedPair>,
    /// Randomized candidate-to-condition mappings.
    pub entries: Vec<RestrictedPairingEntry>,
}

/// One paired task instance and its present or missing variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestrictedPair {
    /// Opaque paired-input identity.
    pub pair_id: String,
    /// Exact task digest shared by all members.
    pub task_sha256: String,
    /// Exact deterministic trajectory seed.
    pub deterministic_seed: u64,
    /// Digest of shared bounds, stopping, and retry rules.
    pub budget_sha256: String,
    /// Variants absent from the source package.
    pub missing_variants: Vec<AblationVariant>,
}

/// One restricted condition identity for an opaque candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestrictedPairingEntry {
    /// Opaque candidate identity visible to graders.
    pub blind_id: String,
    /// Opaque paired-input identity.
    pub pair_id: String,
    /// Original source-free trajectory attempt identity.
    pub attempt_id: String,
    /// Condition identity hidden during grading.
    pub variant: AblationVariant,
    /// Keyed ordering commitment used to verify randomized presentation.
    pub order_sha256: String,
}

/// Randomized candidates plus their separately handled pairing map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedBlindedAblation {
    /// Candidate records safe to expose to automated graders.
    pub candidates: Vec<BlindedAblationCandidate>,
    /// Restricted map that must remain hidden until grades are final.
    pub pairing_map: RestrictedPairingMap,
}

impl PreparedBlindedAblation {
    /// Adds one measured direct-retrieval execution to its preregistered pair.
    ///
    /// The caller must supply the exact observed tool identities and source-free
    /// counters while raw request, response, and source frames remain ephemeral.
    ///
    /// # Errors
    ///
    /// Returns [`AblationError`] when the protocol or key differs, the attempt
    /// is outside the preregistered pairs, the direct tool set or counters
    /// violate shared bounds, or the pair already contains a direct candidate.
    pub fn add_direct_sequence_measurement(
        &mut self,
        protocol: &AblationProtocol,
        blinding_key: &AblationBlindingKey,
        attempt_index: u16,
        outcome: BlindedRunOutcome,
        metrics: BlindedCandidateMetrics,
        observed_tools: &[String],
    ) -> Result<String, AblationError> {
        protocol.validate()?;
        validate_prepared(protocol, self)?;
        if protocol.blinding_key_sha256 != blinding_key.commitment_sha256() {
            return Err(AblationError::ProtocolBindingMismatch);
        }
        let direct = protocol
            .variants
            .iter()
            .find(|variant| variant.variant == AblationVariant::DirectSequence)
            .ok_or(AblationError::InvalidProtocol)?;
        let mut canonical_tools = observed_tools.to_vec();
        canonical_tools.sort();
        if canonical_tools.windows(2).any(|tools| tools[0] == tools[1])
            || canonical_tools != direct.required_tools
            || metrics.calls
                != u64::try_from(observed_tools.len())
                    .map_err(|_| AblationError::CounterOverflow)?
            || metrics.calls == 0
            || metrics.calls > u64::from(protocol.shared_bounds.tool_calls)
            || metrics.tokens > protocol.shared_bounds.total_tokens
            || metrics.source_tokens > metrics.tokens
            || metrics.elapsed_ns > protocol.shared_bounds.elapsed_ns
        {
            return Err(AblationError::InvalidDirectMeasurement);
        }
        let pair = self
            .pairing_map
            .pairs
            .get_mut(usize::from(attempt_index))
            .ok_or(AblationError::InvalidDirectMeasurement)?;
        let expected_seed = *protocol
            .attempt_seeds
            .get(usize::from(attempt_index))
            .ok_or(AblationError::InvalidDirectMeasurement)?;
        if pair.deterministic_seed != expected_seed
            || !pair
                .missing_variants
                .contains(&AblationVariant::DirectSequence)
            || self.pairing_map.entries.iter().any(|entry| {
                entry.pair_id == pair.pair_id && entry.variant == AblationVariant::DirectSequence
            })
        {
            return Err(AblationError::InvalidDirectMeasurement);
        }
        let attempt_id = format!("{CONTEXT_WORKFLOW_ID}-direct_sequence-{attempt_index:02}");
        let blind_id = opaque_id(
            "candidate",
            blinding_key,
            &[
                self.pairing_map.protocol_sha256.as_bytes(),
                attempt_id.as_bytes(),
                &expected_seed.to_le_bytes(),
            ],
        );
        if self
            .candidates
            .iter()
            .any(|candidate| candidate.blind_id == blind_id)
        {
            return Err(AblationError::BlindIdCollision);
        }
        let candidate_sha256 = digest_json(
            "rootlight.ablation.candidate.v1",
            &(
                blind_id.as_str(),
                pair.pair_id.as_str(),
                pair.task_sha256.as_str(),
                outcome,
                metrics,
            ),
        )?;
        self.candidates.push(BlindedAblationCandidate {
            blind_id: blind_id.clone(),
            pair_id: pair.pair_id.clone(),
            task_sha256: pair.task_sha256.clone(),
            outcome,
            metrics,
            candidate_sha256,
        });
        self.pairing_map.entries.push(RestrictedPairingEntry {
            blind_id: blind_id.clone(),
            pair_id: pair.pair_id.clone(),
            attempt_id,
            variant: AblationVariant::DirectSequence,
            order_sha256: hex_digest(&randomized_order_key(
                protocol.randomization_seed,
                blinding_key,
                &blind_id,
            )),
        });
        pair.missing_variants
            .retain(|variant| *variant != AblationVariant::DirectSequence);
        self.candidates.sort_by_key(|candidate| {
            randomized_order_key(
                protocol.randomization_seed,
                blinding_key,
                &candidate.blind_id,
            )
        });
        self.pairing_map
            .entries
            .sort_by(|left, right| left.order_sha256.cmp(&right.order_sha256));
        validate_prepared(protocol, self)?;
        Ok(blind_id)
    }

    /// Finalizes automated grades and aggregate decisions for prepared evidence.
    ///
    /// # Errors
    ///
    /// Returns [`AblationError`] when prepared candidates, rubric observations,
    /// automated grades, pairing, or aggregate reconciliation is invalid.
    pub fn evaluate(
        self,
        protocol: AblationProtocol,
        rubric_evidence: Vec<CandidateRubricEvidence>,
    ) -> Result<ContextPackAblationEvidence, AblationError> {
        protocol.validate()?;
        validate_prepared(&protocol, &self)?;
        let (raw_automated_grades, automated_adjudications, final_automated_grades, agreement) =
            grade_candidates(&self.candidates, &rubric_evidence)?;
        let aggregate = aggregate_report(
            &protocol,
            &self.candidates,
            &self.pairing_map,
            &final_automated_grades,
            &rubric_evidence,
        )?;
        let evidence = ContextPackAblationEvidence {
            protocol,
            blinded_candidates: self.candidates,
            rubric_evidence,
            raw_automated_grades,
            automated_agreement: agreement,
            automated_adjudications,
            final_automated_grades,
            aggregate,
            restricted_pairing_map: self.pairing_map,
        };
        evidence.validate()?;
        Ok(evidence)
    }
}

/// Builds opaque, randomized candidates and a separate restricted pairing map.
///
/// # Errors
///
/// Returns [`AblationError`] when package/protocol/key bindings differ,
/// counters overflow, selectors are ambiguous, or blinding identities collide.
pub fn prepare_blinded_ablation(
    package: &TrajectoryEvidencePackage,
    protocol: &AblationProtocol,
    blinding_key: &AblationBlindingKey,
) -> Result<PreparedBlindedAblation, AblationError> {
    validate_protocol_binding(package, protocol, blinding_key)?;
    let protocol_sha256 = protocol.digest()?;
    let budget_sha256 = digest_json(
        "rootlight.ablation.shared-budget.v1",
        &(
            protocol.shared_bounds,
            protocol.stopping,
            protocol.retry_policy_sha256.as_str(),
        ),
    )?;
    let mut candidates = Vec::new();
    let mut entries = Vec::new();
    let mut pairs = Vec::with_capacity(protocol.attempt_seeds.len());
    let mut blind_ids = BTreeSet::new();
    for (attempt_index, seed) in protocol.attempt_seeds.iter().copied().enumerate() {
        let attempt_index =
            u16::try_from(attempt_index).map_err(|_| AblationError::CounterOverflow)?;
        let task_sha256 = package
            .protocol
            .task_digest(CONTEXT_WORKFLOW_ID)
            .map_err(AblationError::Trajectory)?;
        let pair_id = opaque_id(
            "pair",
            blinding_key,
            &[
                protocol_sha256.as_bytes(),
                task_sha256.as_bytes(),
                &seed.to_le_bytes(),
            ],
        );
        let mut present = BTreeSet::new();
        for variant in &protocol.variants {
            let Some(attempt) = package.attempts.iter().find(|attempt| {
                attempt.workflow_id == variant.workflow_id
                    && attempt.attempt_index == attempt_index
                    && attempt.condition == variant.condition
                    && variant
                        .required_tools
                        .iter()
                        .all(|tool| attempt.calls.iter().any(|call| &call.tool.tool_id == tool))
            }) else {
                continue;
            };
            if !present.insert(variant.variant) {
                return Err(AblationError::AmbiguousVariant);
            }
            let blind_id = opaque_id(
                "candidate",
                blinding_key,
                &[
                    protocol_sha256.as_bytes(),
                    attempt.attempt_id.as_bytes(),
                    &seed.to_le_bytes(),
                ],
            );
            if !blind_ids.insert(blind_id.clone()) {
                return Err(AblationError::BlindIdCollision);
            }
            let metrics = attempt_metrics(attempt)?;
            let outcome = BlindedRunOutcome::from(&attempt.outcome);
            let candidate_sha256 = digest_json(
                "rootlight.ablation.candidate.v1",
                &(
                    blind_id.as_str(),
                    pair_id.as_str(),
                    task_sha256.as_str(),
                    outcome,
                    metrics,
                ),
            )?;
            candidates.push(BlindedAblationCandidate {
                blind_id: blind_id.clone(),
                pair_id: pair_id.clone(),
                task_sha256: task_sha256.clone(),
                outcome,
                metrics,
                candidate_sha256,
            });
            entries.push(RestrictedPairingEntry {
                blind_id,
                pair_id: pair_id.clone(),
                attempt_id: attempt.attempt_id.clone(),
                variant: variant.variant,
                order_sha256: hex_digest(&randomized_order_key(
                    protocol.randomization_seed,
                    blinding_key,
                    candidates
                        .last()
                        .ok_or(AblationError::PairingMismatch)?
                        .blind_id
                        .as_str(),
                )),
            });
        }
        pairs.push(RestrictedPair {
            pair_id,
            task_sha256,
            deterministic_seed: seed,
            budget_sha256: budget_sha256.clone(),
            missing_variants: AblationVariant::ALL
                .into_iter()
                .filter(|variant| !present.contains(variant))
                .collect(),
        });
    }
    candidates.sort_by_key(|candidate| {
        randomized_order_key(
            protocol.randomization_seed,
            blinding_key,
            &candidate.blind_id,
        )
    });
    entries.sort_by(|left, right| left.order_sha256.cmp(&right.order_sha256));
    let prepared = PreparedBlindedAblation {
        candidates,
        pairing_map: RestrictedPairingMap {
            protocol_sha256,
            pairs,
            entries,
        },
    };
    validate_prepared(protocol, &prepared)?;
    Ok(prepared)
}

fn validate_protocol_binding(
    package: &TrajectoryEvidencePackage,
    protocol: &AblationProtocol,
    blinding_key: &AblationBlindingKey,
) -> Result<(), AblationError> {
    package.validate()?;
    protocol.validate()?;
    if protocol.trajectory_protocol_sha256 != package.digests.protocol_sha256
        || protocol.shared_bounds != package.protocol.bounds
        || protocol.stopping != package.protocol.stopping
        || protocol.attempt_seeds != package.protocol.attempt_seeds
        || protocol.blinding_key_sha256 != blinding_key.commitment_sha256()
    {
        return Err(AblationError::ProtocolBindingMismatch);
    }
    let expected =
        preregister_context_pack_ablation(package, blinding_key, &protocol.source_revision)?;
    if &expected != protocol {
        return Err(AblationError::ProtocolBindingMismatch);
    }
    Ok(())
}

fn validate_prepared(
    protocol: &AblationProtocol,
    prepared: &PreparedBlindedAblation,
) -> Result<(), AblationError> {
    if prepared.pairing_map.protocol_sha256 != protocol.digest()?
        || prepared.pairing_map.pairs.len() != protocol.attempt_seeds.len()
        || prepared.pairing_map.entries.len() != prepared.candidates.len()
    {
        return Err(AblationError::PairingMismatch);
    }
    let candidate_ids = prepared
        .candidates
        .iter()
        .map(|candidate| candidate.blind_id.as_str())
        .collect::<BTreeSet<_>>();
    let entry_ids = prepared
        .pairing_map
        .entries
        .iter()
        .map(|entry| entry.blind_id.as_str())
        .collect::<BTreeSet<_>>();
    if candidate_ids.len() != prepared.candidates.len()
        || entry_ids.len() != prepared.pairing_map.entries.len()
        || candidate_ids != entry_ids
    {
        return Err(AblationError::PairingMismatch);
    }
    let pair_ids = prepared
        .pairing_map
        .pairs
        .iter()
        .map(|pair| pair.pair_id.as_str())
        .collect::<BTreeSet<_>>();
    if pair_ids.len() != prepared.pairing_map.pairs.len() {
        return Err(AblationError::PairingMismatch);
    }
    for (index, pair) in prepared.pairing_map.pairs.iter().enumerate() {
        validate_label(&pair.pair_id)?;
        validate_digest(&pair.task_sha256)?;
        validate_digest(&pair.budget_sha256)?;
        if pair.deterministic_seed
            != *protocol
                .attempt_seeds
                .get(index)
                .ok_or(AblationError::PairingMismatch)?
            || pair
                .missing_variants
                .windows(2)
                .any(|values| values[0] >= values[1])
        {
            return Err(AblationError::PairingMismatch);
        }
        let present = prepared
            .pairing_map
            .entries
            .iter()
            .filter(|entry| entry.pair_id == pair.pair_id)
            .map(|entry| entry.variant)
            .collect::<BTreeSet<_>>();
        let expected_missing = AblationVariant::ALL
            .into_iter()
            .filter(|variant| !present.contains(variant))
            .collect::<Vec<_>>();
        if expected_missing != pair.missing_variants {
            return Err(AblationError::PairingMismatch);
        }
    }
    for (candidate, entry) in prepared
        .candidates
        .iter()
        .zip(&prepared.pairing_map.entries)
    {
        validate_label(&entry.blind_id)?;
        validate_label(&entry.pair_id)?;
        validate_label(&entry.attempt_id)?;
        validate_digest(&entry.order_sha256)?;
        if candidate.blind_id != entry.blind_id
            || candidate.pair_id != entry.pair_id
            || !pair_ids.contains(entry.pair_id.as_str())
        {
            return Err(AblationError::PairingMismatch);
        }
    }
    if prepared
        .pairing_map
        .entries
        .windows(2)
        .any(|entries| entries[0].order_sha256 >= entries[1].order_sha256)
    {
        return Err(AblationError::PairingMismatch);
    }
    for candidate in &prepared.candidates {
        validate_label(&candidate.blind_id)?;
        validate_label(&candidate.pair_id)?;
        validate_digest(&candidate.task_sha256)?;
        validate_digest(&candidate.candidate_sha256)?;
        let expected = digest_json(
            "rootlight.ablation.candidate.v1",
            &(
                candidate.blind_id.as_str(),
                candidate.pair_id.as_str(),
                candidate.task_sha256.as_str(),
                candidate.outcome,
                candidate.metrics,
            ),
        )?;
        if expected != candidate.candidate_sha256 {
            return Err(AblationError::DigestMismatch);
        }
    }
    Ok(())
}

fn attempt_metrics(
    attempt: &crate::TrajectoryAttemptRecord,
) -> Result<BlindedCandidateMetrics, AblationError> {
    let mut metrics = BlindedCandidateMetrics {
        calls: u64::try_from(attempt.calls.len()).map_err(|_| AblationError::CounterOverflow)?,
        unsupported_claims: attempt.claim_signals.unsupported_claims,
        ..BlindedCandidateMetrics::default()
    };
    for call in &attempt.calls {
        metrics.tokens = metrics
            .tokens
            .checked_add(
                call.accounting
                    .total
                    .actual_tokens
                    .ok_or(AblationError::MissingActualTokens)?,
            )
            .ok_or(AblationError::CounterOverflow)?;
        metrics.source_tokens = metrics
            .source_tokens
            .checked_add(
                call.accounting
                    .source
                    .actual_tokens
                    .ok_or(AblationError::MissingActualTokens)?,
            )
            .ok_or(AblationError::CounterOverflow)?;
        metrics.elapsed_ns = metrics
            .elapsed_ns
            .checked_add(call.elapsed_ns)
            .ok_or(AblationError::CounterOverflow)?;
    }
    Ok(metrics)
}

/// Machine-observable checks or an explicit unsupported grading dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum RubricObservation {
    /// Bounded deterministic checks supporting an automated score.
    Checks {
        /// Individual source-free check outcomes.
        checks: Vec<bool>,
    },
    /// The retained evidence cannot support this qualitative dimension.
    Unsupported {
        /// Stable source-free reason.
        reason_code: String,
    },
}

/// Closed unsupported-claim classes required by the blinded rubric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedClaimCategory {
    /// Claim conflicts with retained evidence.
    ContradictedByEvidence,
    /// Claim treats truncated or partial results as complete.
    PartialOrTruncatedResult,
    /// Claim fabricates or misattributes source support.
    FabricatedSourceSupport,
    /// Negative claim exceeds the searched evidence universe.
    OverbroadNegativeClaim,
}

impl UnsupportedClaimCategory {
    const ALL: [Self; 4] = [
        Self::ContradictedByEvidence,
        Self::PartialOrTruncatedResult,
        Self::FabricatedSourceSupport,
        Self::OverbroadNegativeClaim,
    ];
}

/// Unsupported-claim assessment kept distinct from quality scoring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum UnsupportedClaimAssessment {
    /// Complete automated category counts.
    Assessed {
        /// Counts for all observed unsupported claims.
        categories: BTreeMap<UnsupportedClaimCategory, u32>,
    },
    /// Answer evidence cannot support claim assessment.
    Unsupported {
        /// Stable source-free reason.
        reason_code: String,
    },
}

/// Source-free rubric observations for one blinded candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRubricEvidence {
    /// Opaque candidate identity.
    pub blind_id: String,
    /// Candidate digest preventing evidence reuse across candidates.
    pub candidate_sha256: String,
    /// Complete seven-dimension observation map.
    pub observations: BTreeMap<RubricDimension, RubricObservation>,
    /// Separate unsupported-claim category assessment.
    pub unsupported_claims: UnsupportedClaimAssessment,
}

impl CandidateRubricEvidence {
    /// Creates explicit unsupported observations for source-free trajectories
    /// that retain no answer content.
    #[must_use]
    pub fn answer_content_not_retained(candidate: &BlindedAblationCandidate) -> Self {
        Self {
            blind_id: candidate.blind_id.clone(),
            candidate_sha256: candidate.candidate_sha256.clone(),
            observations: RubricDimension::ALL
                .into_iter()
                .map(|dimension| {
                    (
                        dimension,
                        RubricObservation::Unsupported {
                            reason_code: "answer_content_not_retained".to_owned(),
                        },
                    )
                })
                .collect(),
            unsupported_claims: UnsupportedClaimAssessment::Unsupported {
                reason_code: "answer_content_not_retained".to_owned(),
            },
        }
    }

    fn validate(&self, candidate: &BlindedAblationCandidate) -> Result<(), AblationError> {
        validate_label(&self.blind_id)?;
        validate_digest(&self.candidate_sha256)?;
        if self.blind_id != candidate.blind_id
            || self.candidate_sha256 != candidate.candidate_sha256
            || self.observations.len() != RubricDimension::ALL.len()
        {
            return Err(AblationError::InvalidRubricEvidence);
        }
        for dimension in RubricDimension::ALL {
            let observation = self
                .observations
                .get(&dimension)
                .ok_or(AblationError::InvalidRubricEvidence)?;
            match observation {
                RubricObservation::Checks { checks }
                    if checks.is_empty() || checks.len() > MAX_CHECKS_PER_DIMENSION =>
                {
                    return Err(AblationError::InvalidRubricEvidence);
                }
                RubricObservation::Unsupported { reason_code } => {
                    validate_label(reason_code)?;
                }
                RubricObservation::Checks { .. } => {}
            }
        }
        match &self.unsupported_claims {
            UnsupportedClaimAssessment::Assessed { categories } => {
                let total = categories.values().try_fold(0_u32, |total, count| {
                    total
                        .checked_add(*count)
                        .ok_or(AblationError::CounterOverflow)
                })?;
                if categories
                    .keys()
                    .any(|category| !UnsupportedClaimCategory::ALL.contains(category))
                    || total != candidate.metrics.unsupported_claims
                {
                    return Err(AblationError::InvalidRubricEvidence);
                }
            }
            UnsupportedClaimAssessment::Unsupported { reason_code } => {
                validate_label(reason_code)?;
            }
        }
        Ok(())
    }
}

/// Explicit identity class for a grade producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraderKind {
    /// Deterministic automated rules, never a human rating.
    Automated,
}

/// One automated grader identity retained with every raw grade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomatedGraderIdentity {
    /// Explicit automated identity class.
    pub kind: GraderKind,
    /// Stable algorithm identity.
    pub grader_id: String,
}

/// One dimension-level automated grade or explicit unsupported disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum DimensionGrade {
    /// Score in hundredths of one point on a zero-to-one-hundred scale.
    Scored {
        /// Integer score from zero through ten thousand.
        score_centipoints: u16,
    },
    /// Evidence could not support a score.
    Unsupported {
        /// Stable source-free reason.
        reason_code: String,
    },
}

/// Final candidate-level grading disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidateGrade {
    /// Fully supported weighted rubric score.
    Scored {
        /// Score in hundredths of one point.
        score_centipoints: u16,
    },
    /// At least one qualitative dimension was unsupported.
    Unsupported {
        /// Stable source-free reason.
        reason_code: String,
    },
    /// Failed run retained as zero quality under preregistered rules.
    Failed,
    /// Timed-out run retained as zero quality under preregistered rules.
    TimedOut,
    /// Cancelled run retained as zero quality under preregistered rules.
    Cancelled,
    /// Unsupported execution retained as zero quality under preregistered rules.
    UnsupportedExecution,
    /// Optional executable absence remains ungraded in sensitivity bounds.
    NotAvailable,
    /// Preregistered exclusion remains ungraded in sensitivity bounds.
    Excluded,
}

/// One complete raw automated grade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomatedRawGrade {
    /// Opaque candidate identity.
    pub blind_id: String,
    /// Explicit automated grader identity.
    pub grader: AutomatedGraderIdentity,
    /// Canonically ordered dimension grades.
    pub dimensions: BTreeMap<RubricDimension, DimensionGrade>,
    /// Weighted overall disposition.
    pub overall: CandidateGrade,
}

/// Inter-rater agreement from the two deterministic automated graders.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomatedAgreement {
    /// Dimension comparisons, including matching unsupported dispositions.
    pub dimension_comparisons: u32,
    /// Exactly matching dimension grades.
    pub exact_dimension_agreements: u32,
    /// Candidates with comparable overall scores.
    pub overall_score_comparisons: u32,
    /// Exactly matching overall scores.
    pub exact_overall_agreements: u32,
    /// Sum of absolute overall score differences.
    pub overall_absolute_difference_centipoints: u64,
}

/// One bounded automated adjudication; no human judgment is implied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomatedAdjudicationRecord {
    /// Explicit automated adjudicator identity.
    pub adjudicator: AutomatedGraderIdentity,
    /// Opaque candidate identity.
    pub blind_id: String,
    /// Disputed rubric dimension.
    pub dimension: RubricDimension,
    /// First raw grade.
    pub grader_a: DimensionGrade,
    /// Second raw grade.
    pub grader_b: DimensionGrade,
    /// Conservative deterministic resolution.
    pub resolved: DimensionGrade,
}

/// One finalized blinded candidate grade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalAutomatedGrade {
    /// Opaque candidate identity.
    pub blind_id: String,
    /// Canonically ordered final dimension grades.
    pub dimensions: BTreeMap<RubricDimension, DimensionGrade>,
    /// Final weighted grade or explicit non-gradable state.
    pub overall: CandidateGrade,
}

type AutomatedGradingResult = (
    Vec<AutomatedRawGrade>,
    Vec<AutomatedAdjudicationRecord>,
    Vec<FinalAutomatedGrade>,
    AutomatedAgreement,
);

fn grade_candidates(
    candidates: &[BlindedAblationCandidate],
    evidence: &[CandidateRubricEvidence],
) -> Result<AutomatedGradingResult, AblationError> {
    let evidence_by_id = evidence
        .iter()
        .map(|record| (record.blind_id.as_str(), record))
        .collect::<BTreeMap<_, _>>();
    if evidence_by_id.len() != evidence.len() {
        return Err(AblationError::DuplicateRubricEvidence);
    }
    if evidence_by_id.keys().any(|blind_id| {
        !candidates
            .iter()
            .any(|candidate| &candidate.blind_id == blind_id)
    }) {
        return Err(AblationError::UnknownBlindId);
    }
    let mut raw = Vec::with_capacity(candidates.len() * 2);
    let mut adjudications = Vec::new();
    let mut final_grades = Vec::with_capacity(candidates.len());
    let mut agreement = AutomatedAgreement::default();
    for candidate in candidates {
        let default_evidence;
        let rubric_evidence = if candidate.outcome == BlindedRunOutcome::Succeeded {
            match evidence_by_id.get(candidate.blind_id.as_str()).copied() {
                Some(evidence) => evidence,
                None => {
                    default_evidence =
                        CandidateRubricEvidence::answer_content_not_retained(candidate);
                    &default_evidence
                }
            }
        } else {
            default_evidence = CandidateRubricEvidence::answer_content_not_retained(candidate);
            &default_evidence
        };
        rubric_evidence.validate(candidate)?;
        let grade_a = automated_grade_mean(candidate, rubric_evidence)?;
        let grade_b = automated_grade_strict(candidate, rubric_evidence)?;
        update_agreement(&mut agreement, &grade_a, &grade_b)?;
        let (final_grade, mut candidate_adjudications) =
            adjudicate_automated_grades(candidate, &grade_a, &grade_b)?;
        if candidate_adjudications.len() > 7 {
            return Err(AblationError::AdjudicationLimitExceeded);
        }
        adjudications.append(&mut candidate_adjudications);
        raw.push(grade_a);
        raw.push(grade_b);
        final_grades.push(final_grade);
    }
    Ok((raw, adjudications, final_grades, agreement))
}

fn automated_grade_mean(
    candidate: &BlindedAblationCandidate,
    evidence: &CandidateRubricEvidence,
) -> Result<AutomatedRawGrade, AblationError> {
    automated_grade(candidate, evidence, AUTOMATED_GRADER_A_ID, |checks| {
        let passed = checks.iter().filter(|value| **value).count();
        ratio_score(passed, checks.len())
    })
}

fn automated_grade_strict(
    candidate: &BlindedAblationCandidate,
    evidence: &CandidateRubricEvidence,
) -> Result<AutomatedRawGrade, AblationError> {
    automated_grade(candidate, evidence, AUTOMATED_GRADER_B_ID, |checks| {
        if checks.iter().all(|value| *value) {
            Ok(MAX_QUALITY_SCORE_CENTIPOINTS)
        } else {
            Ok(0)
        }
    })
}

fn automated_grade(
    candidate: &BlindedAblationCandidate,
    evidence: &CandidateRubricEvidence,
    grader_id: &str,
    score_checks: impl Fn(&[bool]) -> Result<u16, AblationError>,
) -> Result<AutomatedRawGrade, AblationError> {
    let dimensions = if candidate.outcome == BlindedRunOutcome::Succeeded {
        let mut dimensions = BTreeMap::new();
        for dimension in RubricDimension::ALL {
            let grade = match evidence
                .observations
                .get(&dimension)
                .ok_or(AblationError::InvalidRubricEvidence)?
            {
                RubricObservation::Checks { checks } => DimensionGrade::Scored {
                    score_centipoints: score_checks(checks)?,
                },
                RubricObservation::Unsupported { reason_code } => DimensionGrade::Unsupported {
                    reason_code: reason_code.clone(),
                },
            };
            dimensions.insert(dimension, grade);
        }
        dimensions
    } else {
        BTreeMap::new()
    };
    let overall = candidate_grade_from_dimensions(candidate.outcome, &dimensions)?;
    Ok(AutomatedRawGrade {
        blind_id: candidate.blind_id.clone(),
        grader: AutomatedGraderIdentity {
            kind: GraderKind::Automated,
            grader_id: grader_id.to_owned(),
        },
        dimensions,
        overall,
    })
}

fn candidate_grade_from_dimensions(
    outcome: BlindedRunOutcome,
    dimensions: &BTreeMap<RubricDimension, DimensionGrade>,
) -> Result<CandidateGrade, AblationError> {
    match outcome {
        BlindedRunOutcome::Failed => return Ok(CandidateGrade::Failed),
        BlindedRunOutcome::TimedOut => return Ok(CandidateGrade::TimedOut),
        BlindedRunOutcome::Cancelled => return Ok(CandidateGrade::Cancelled),
        BlindedRunOutcome::Unsupported => return Ok(CandidateGrade::UnsupportedExecution),
        BlindedRunOutcome::NotAvailable => return Ok(CandidateGrade::NotAvailable),
        BlindedRunOutcome::Excluded => return Ok(CandidateGrade::Excluded),
        BlindedRunOutcome::Succeeded => {}
    }
    if dimensions.len() != RubricDimension::ALL.len() {
        return Err(AblationError::InvalidGrade);
    }
    let mut weighted = 0_u64;
    for dimension in RubricDimension::ALL {
        match dimensions
            .get(&dimension)
            .ok_or(AblationError::InvalidGrade)?
        {
            DimensionGrade::Scored { score_centipoints } => {
                if *score_centipoints > MAX_QUALITY_SCORE_CENTIPOINTS {
                    return Err(AblationError::InvalidGrade);
                }
                weighted = weighted
                    .checked_add(
                        u64::from(*score_centipoints)
                            .checked_mul(u64::from(dimension.weight()))
                            .ok_or(AblationError::CounterOverflow)?,
                    )
                    .ok_or(AblationError::CounterOverflow)?;
            }
            DimensionGrade::Unsupported { reason_code } => {
                validate_label(reason_code)?;
                return Ok(CandidateGrade::Unsupported {
                    reason_code: reason_code.clone(),
                });
            }
        }
    }
    Ok(CandidateGrade::Scored {
        score_centipoints: u16::try_from(weighted / 100)
            .map_err(|_| AblationError::CounterOverflow)?,
    })
}

fn update_agreement(
    agreement: &mut AutomatedAgreement,
    grade_a: &AutomatedRawGrade,
    grade_b: &AutomatedRawGrade,
) -> Result<(), AblationError> {
    for dimension in RubricDimension::ALL {
        let left = grade_a.dimensions.get(&dimension);
        let right = grade_b.dimensions.get(&dimension);
        if let (Some(left), Some(right)) = (left, right) {
            agreement.dimension_comparisons = agreement
                .dimension_comparisons
                .checked_add(1)
                .ok_or(AblationError::CounterOverflow)?;
            agreement.exact_dimension_agreements = agreement
                .exact_dimension_agreements
                .checked_add(u32::from(left == right))
                .ok_or(AblationError::CounterOverflow)?;
        }
    }
    if let (
        CandidateGrade::Scored {
            score_centipoints: left,
        },
        CandidateGrade::Scored {
            score_centipoints: right,
        },
    ) = (&grade_a.overall, &grade_b.overall)
    {
        agreement.overall_score_comparisons = agreement
            .overall_score_comparisons
            .checked_add(1)
            .ok_or(AblationError::CounterOverflow)?;
        agreement.exact_overall_agreements = agreement
            .exact_overall_agreements
            .checked_add(u32::from(left == right))
            .ok_or(AblationError::CounterOverflow)?;
        agreement.overall_absolute_difference_centipoints = agreement
            .overall_absolute_difference_centipoints
            .checked_add(u64::from(left.abs_diff(*right)))
            .ok_or(AblationError::CounterOverflow)?;
    }
    Ok(())
}

fn adjudicate_automated_grades(
    candidate: &BlindedAblationCandidate,
    grade_a: &AutomatedRawGrade,
    grade_b: &AutomatedRawGrade,
) -> Result<(FinalAutomatedGrade, Vec<AutomatedAdjudicationRecord>), AblationError> {
    if candidate.outcome != BlindedRunOutcome::Succeeded {
        if grade_a.overall != grade_b.overall {
            return Err(AblationError::InvalidGrade);
        }
        return Ok((
            FinalAutomatedGrade {
                blind_id: candidate.blind_id.clone(),
                dimensions: BTreeMap::new(),
                overall: grade_a.overall.clone(),
            },
            Vec::new(),
        ));
    }
    let mut dimensions = BTreeMap::new();
    let mut adjudications = Vec::new();
    for dimension in RubricDimension::ALL {
        let left = grade_a
            .dimensions
            .get(&dimension)
            .ok_or(AblationError::InvalidGrade)?;
        let right = grade_b
            .dimensions
            .get(&dimension)
            .ok_or(AblationError::InvalidGrade)?;
        let resolved = if left == right {
            left.clone()
        } else {
            let resolved = conservative_dimension_grade(left, right)?;
            adjudications.push(AutomatedAdjudicationRecord {
                adjudicator: AutomatedGraderIdentity {
                    kind: GraderKind::Automated,
                    grader_id: AUTOMATED_ADJUDICATOR_ID.to_owned(),
                },
                blind_id: candidate.blind_id.clone(),
                dimension,
                grader_a: left.clone(),
                grader_b: right.clone(),
                resolved: resolved.clone(),
            });
            resolved
        };
        dimensions.insert(dimension, resolved);
    }
    let overall = candidate_grade_from_dimensions(candidate.outcome, &dimensions)?;
    Ok((
        FinalAutomatedGrade {
            blind_id: candidate.blind_id.clone(),
            dimensions,
            overall,
        },
        adjudications,
    ))
}

fn conservative_dimension_grade(
    left: &DimensionGrade,
    right: &DimensionGrade,
) -> Result<DimensionGrade, AblationError> {
    match (left, right) {
        (
            DimensionGrade::Scored {
                score_centipoints: left,
            },
            DimensionGrade::Scored {
                score_centipoints: right,
            },
        ) => Ok(DimensionGrade::Scored {
            score_centipoints: (*left).min(*right),
        }),
        (DimensionGrade::Unsupported { reason_code }, DimensionGrade::Scored { .. })
        | (DimensionGrade::Scored { .. }, DimensionGrade::Unsupported { reason_code }) => {
            Ok(DimensionGrade::Unsupported {
                reason_code: reason_code.clone(),
            })
        }
        (
            DimensionGrade::Unsupported { reason_code: left },
            DimensionGrade::Unsupported { reason_code: right },
        ) if left == right => Ok(DimensionGrade::Unsupported {
            reason_code: left.clone(),
        }),
        (DimensionGrade::Unsupported { .. }, DimensionGrade::Unsupported { .. }) => {
            Err(AblationError::ConflictingUnsupportedReasons)
        }
    }
}

fn ratio_score(passed: usize, total: usize) -> Result<u16, AblationError> {
    let passed = u64::try_from(passed).map_err(|_| AblationError::CounterOverflow)?;
    let total = u64::try_from(total).map_err(|_| AblationError::CounterOverflow)?;
    let scaled = passed
        .checked_mul(u64::from(MAX_QUALITY_SCORE_CENTIPOINTS))
        .ok_or(AblationError::CounterOverflow)?
        .checked_div(total)
        .ok_or(AblationError::InvalidRubricEvidence)?;
    u16::try_from(scaled).map_err(|_| AblationError::CounterOverflow)
}

/// Complete attempt accounting for one ablation variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantAggregate {
    /// Variant identity disclosed only after automated grades are final.
    pub variant: AblationVariant,
    /// Expected paired task instances.
    pub expected_attempts: u32,
    /// Present attempts, including failures and exclusions.
    pub observed_attempts: u32,
    /// Successful executions.
    pub succeeded: u32,
    /// Successful execution rate per million observed attempts.
    pub task_success_rate_ppm: Option<u32>,
    /// Failed executions.
    pub failed: u32,
    /// Timed-out executions.
    pub timed_out: u32,
    /// Cancelled executions.
    pub cancelled: u32,
    /// Unsupported executions.
    pub unsupported: u32,
    /// Unavailable optional executables.
    pub not_available: u32,
    /// Preregistered exclusions.
    pub excluded: u32,
    /// Attempts with fully supported quality grades.
    pub quality_graded: u32,
    /// Mean finalized quality score when any supported grades exist.
    pub mean_quality_centipoints: Option<u16>,
    /// Attempts containing one or more unsupported claims.
    pub attempts_with_unsupported_claims: u32,
    /// Attempts with a complete unsupported-claim assessment.
    pub unsupported_claim_assessed: u32,
    /// Unsupported-claim attempt rate per million assessed attempts.
    pub unsupported_claim_rate_ppm: Option<u32>,
    /// Aggregate counts by closed unsupported-claim category.
    pub unsupported_claim_categories: BTreeMap<UnsupportedClaimCategory, u32>,
    /// Raw resource totals; no gain claim is implied.
    pub resource_totals: BlindedCandidateMetrics,
}

/// Sensitivity bounds under preregistered treatment of missing grades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualitySensitivity {
    /// Observed primary mean difference when any complete pairs exist.
    pub observed_difference_centipoints: Option<i32>,
    /// Worst-case context-minus-direct difference with missing values bounded.
    pub worst_case_difference_centipoints: i32,
    /// Best-case context-minus-direct difference with missing values bounded.
    pub best_case_difference_centipoints: i32,
    /// Missing or excluded context values included in the bounds.
    pub context_ungraded: u32,
    /// Missing or excluded direct values included in the bounds.
    pub direct_ungraded: u32,
}

/// Deterministic paired-bootstrap uncertainty interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedUncertaintyInterval {
    /// Fixed interval algorithm.
    pub method: UncertaintyMethod,
    /// Confidence level in parts per million.
    pub confidence_ppm: u32,
    /// Lower context-minus-direct difference in hundredths of one point.
    pub lower_centipoints: i32,
    /// Upper context-minus-direct difference in hundredths of one point.
    pub upper_centipoints: i32,
    /// Complete paired observations.
    pub paired_observations: u32,
    /// Deterministic bootstrap replicates.
    pub replicates: u32,
}

/// Closed uncertainty method used by the aggregate report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyMethod {
    /// Fixed-seed nonparametric bootstrap over paired differences.
    DeterministicPairedBootstrap,
}

/// Efficiency comparison published only beside a complete quality result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EfficiencyAlongsideQuality {
    /// Mean context-pack calls.
    pub context_calls: u64,
    /// Mean direct-sequence calls.
    pub direct_calls: u64,
    /// Mean context-pack actual tokens.
    pub context_tokens: u64,
    /// Mean direct-sequence actual tokens.
    pub direct_tokens: u64,
    /// Mean context-pack source tokens.
    pub context_source_tokens: u64,
    /// Mean direct-sequence source tokens.
    pub direct_source_tokens: u64,
    /// Mean context-pack latency.
    pub context_elapsed_ns: u64,
    /// Mean direct-sequence latency.
    pub direct_elapsed_ns: u64,
    /// Final context-pack quality.
    pub context_quality_centipoints: u16,
    /// Final direct-sequence quality.
    pub direct_quality_centipoints: u16,
}

/// Explicit context-pack evaluation disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum AblationDecision {
    /// Complete evidence meets quality, unsupported-claim, and value rules.
    Pass,
    /// Complete evidence fails one or more immutable targets.
    Fallback {
        /// Canonically ordered source-free failure reasons.
        reason_codes: Vec<String>,
    },
    /// Evidence cannot support a truthful evaluation result.
    Blocked {
        /// Canonically ordered source-free blocking reasons.
        reason_codes: Vec<String>,
    },
}

/// Reproducible public aggregate after automated grading is finalized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AblationAggregateReport {
    /// Expected context-versus-direct pairs.
    pub expected_pairs: u32,
    /// Pairs with primary context and direct quality values.
    pub complete_quality_pairs: u32,
    /// Paired context-minus-direct quality difference.
    pub paired_quality_difference_centipoints: Option<i32>,
    /// Context quality divided by direct quality in parts per million.
    pub quality_retention_ppm: Option<u32>,
    /// Nonnegative direct-minus-context quality loss.
    pub quality_loss_centipoints: Option<u16>,
    /// Immutable loss threshold.
    pub maximum_quality_loss_centipoints: u16,
    /// Paired quality uncertainty.
    pub uncertainty: Option<PairedUncertaintyInterval>,
    /// Missing/excluded-grade sensitivity bounds.
    pub sensitivity: QualitySensitivity,
    /// Complete variant denominator and quality summaries.
    pub variants: Vec<VariantAggregate>,
    /// Efficiency values, present only when corresponding quality is complete.
    pub efficiency_alongside_quality: Option<EfficiencyAlongsideQuality>,
    /// Explicit evaluation result.
    pub decision: AblationDecision,
}

/// Public source-free grades and aggregates plus the restricted pairing map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPackAblationEvidence {
    /// Complete preregistration frozen before grades.
    pub protocol: AblationProtocol,
    /// Randomized condition-blind candidates.
    pub blinded_candidates: Vec<BlindedAblationCandidate>,
    /// Source-free qualitative observations.
    pub rubric_evidence: Vec<CandidateRubricEvidence>,
    /// Both independent raw automated grades per candidate.
    pub raw_automated_grades: Vec<AutomatedRawGrade>,
    /// Agreement between the two automated graders.
    pub automated_agreement: AutomatedAgreement,
    /// Bounded automated adjudications.
    pub automated_adjudications: Vec<AutomatedAdjudicationRecord>,
    /// Finalized automated candidate grades.
    pub final_automated_grades: Vec<FinalAutomatedGrade>,
    /// Public aggregate and disposition.
    pub aggregate: AblationAggregateReport,
    /// Separately handled condition identity map.
    pub restricted_pairing_map: RestrictedPairingMap,
}

impl ContextPackAblationEvidence {
    /// Validates all protocol, blinding, raw-grade, adjudication, denominator,
    /// sensitivity, aggregate, and decision invariants by recomputation.
    ///
    /// # Errors
    ///
    /// Returns [`AblationError`] when any retained field differs from the
    /// deterministic result derived from embedded source-free evidence.
    pub fn validate(&self) -> Result<(), AblationError> {
        self.protocol.validate()?;
        let prepared = PreparedBlindedAblation {
            candidates: self.blinded_candidates.clone(),
            pairing_map: self.restricted_pairing_map.clone(),
        };
        validate_prepared(&self.protocol, &prepared)?;
        let (raw, adjudications, final_grades, agreement) =
            grade_candidates(&self.blinded_candidates, &self.rubric_evidence)?;
        if raw != self.raw_automated_grades
            || adjudications != self.automated_adjudications
            || final_grades != self.final_automated_grades
            || agreement != self.automated_agreement
        {
            return Err(AblationError::GradeReconciliationMismatch);
        }
        let aggregate = aggregate_report(
            &self.protocol,
            &self.blinded_candidates,
            &self.restricted_pairing_map,
            &self.final_automated_grades,
            &self.rubric_evidence,
        )?;
        if aggregate != self.aggregate {
            return Err(AblationError::AggregateMismatch);
        }
        let encoded = serde_json::to_vec(self).map_err(|_| AblationError::Serialization)?;
        if encoded.len() > MAX_ABLATION_EVIDENCE_BYTES {
            return Err(AblationError::PackageTooLarge);
        }
        Ok(())
    }
}

/// Runs both automated graders, bounded adjudication, and aggregate decisions.
///
/// Missing rubric inputs are not imputed: successful candidates receive the
/// explicit `answer_content_not_retained` unsupported disposition.
///
/// # Errors
///
/// Returns [`AblationError`] when source evidence, preregistration, blinding,
/// observations, grading, pairing, or aggregate reconciliation is invalid.
pub fn evaluate_context_pack_ablation(
    package: &TrajectoryEvidencePackage,
    protocol: AblationProtocol,
    blinding_key: &AblationBlindingKey,
    rubric_evidence: Vec<CandidateRubricEvidence>,
) -> Result<ContextPackAblationEvidence, AblationError> {
    let prepared = prepare_blinded_ablation(package, &protocol, blinding_key)?;
    prepared.evaluate(protocol, rubric_evidence)
}

/// Produces a complete package from an exact source revision and raw rubric evidence.
///
/// This is the bounded orchestration entry point: callers do not need to
/// construct or mutate the preregistration after observing outcomes.
///
/// # Errors
///
/// Returns [`AblationError`] when the revision, trajectory package, protocol,
/// blinding, observations, grading, or aggregate evidence is invalid.
pub fn produce_context_pack_ablation(
    package: &TrajectoryEvidencePackage,
    blinding_key: &AblationBlindingKey,
    source_revision: &str,
    rubric_evidence: Vec<CandidateRubricEvidence>,
) -> Result<ContextPackAblationEvidence, AblationError> {
    let protocol = preregister_context_pack_ablation(package, blinding_key, source_revision)?;
    evaluate_context_pack_ablation(package, protocol, blinding_key, rubric_evidence)
}

/// Encodes validated ablation evidence under a hard byte ceiling.
///
/// # Errors
///
/// Returns [`AblationError`] when validation, serialization, or the encoded
/// byte ceiling fails.
pub fn encode_context_pack_ablation(
    evidence: &ContextPackAblationEvidence,
) -> Result<Vec<u8>, AblationError> {
    evidence.validate()?;
    let encoded = serde_json::to_vec(evidence).map_err(|_| AblationError::Serialization)?;
    if encoded.len() > MAX_ABLATION_EVIDENCE_BYTES {
        return Err(AblationError::PackageTooLarge);
    }
    Ok(encoded)
}

/// Decodes strict bounded ablation evidence and recomputes every aggregate.
///
/// # Errors
///
/// Returns [`AblationError`] for oversized, malformed, unknown-field, or
/// internally inconsistent evidence.
pub fn decode_context_pack_ablation(
    encoded: &[u8],
) -> Result<ContextPackAblationEvidence, AblationError> {
    if encoded.len() > MAX_ABLATION_EVIDENCE_BYTES {
        return Err(AblationError::PackageTooLarge);
    }
    let evidence: ContextPackAblationEvidence =
        serde_json::from_slice(encoded).map_err(|_| AblationError::InvalidEncoding)?;
    evidence.validate()?;
    Ok(evidence)
}

fn aggregate_report(
    protocol: &AblationProtocol,
    candidates: &[BlindedAblationCandidate],
    pairing_map: &RestrictedPairingMap,
    grades: &[FinalAutomatedGrade],
    rubric_evidence: &[CandidateRubricEvidence],
) -> Result<AblationAggregateReport, AblationError> {
    let candidates_by_id = candidates
        .iter()
        .map(|candidate| (candidate.blind_id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    let grades_by_id = grades
        .iter()
        .map(|grade| (grade.blind_id.as_str(), grade))
        .collect::<BTreeMap<_, _>>();
    if candidates_by_id.len() != candidates.len()
        || grades_by_id.len() != grades.len()
        || candidates_by_id.keys().ne(grades_by_id.keys())
    {
        return Err(AblationError::GradeReconciliationMismatch);
    }
    let entries_by_pair = pairing_map.entries.iter().fold(
        BTreeMap::<&str, BTreeMap<AblationVariant, &RestrictedPairingEntry>>::new(),
        |mut groups, entry| {
            groups
                .entry(entry.pair_id.as_str())
                .or_default()
                .insert(entry.variant, entry);
            groups
        },
    );
    let mut differences = Vec::new();
    let mut context_scores = Vec::new();
    let mut direct_scores = Vec::new();
    let mut context_ungraded = 0_u32;
    let mut direct_ungraded = 0_u32;
    let mut blocked_reasons = BTreeSet::new();
    for pair in &pairing_map.pairs {
        let entries = entries_by_pair.get(pair.pair_id.as_str());
        let context = entries.and_then(|entries| entries.get(&AblationVariant::ContextPack));
        let direct = entries.and_then(|entries| entries.get(&AblationVariant::DirectSequence));
        if context.is_none() {
            context_ungraded = context_ungraded
                .checked_add(1)
                .ok_or(AblationError::CounterOverflow)?;
            blocked_reasons.insert("missing_context_pack".to_owned());
        }
        if direct.is_none() {
            direct_ungraded = direct_ungraded
                .checked_add(1)
                .ok_or(AblationError::CounterOverflow)?;
            blocked_reasons.insert("missing_direct_sequence".to_owned());
        }
        let context_score = context.and_then(|entry| {
            grades_by_id
                .get(entry.blind_id.as_str())
                .and_then(|grade| primary_quality_score(&grade.overall))
        });
        let direct_score = direct.and_then(|entry| {
            grades_by_id
                .get(entry.blind_id.as_str())
                .and_then(|grade| primary_quality_score(&grade.overall))
        });
        if context.is_some() && context_score.is_none() {
            context_ungraded = context_ungraded
                .checked_add(1)
                .ok_or(AblationError::CounterOverflow)?;
            blocked_reasons.insert("context_quality_unsupported".to_owned());
        }
        if direct.is_some() && direct_score.is_none() {
            direct_ungraded = direct_ungraded
                .checked_add(1)
                .ok_or(AblationError::CounterOverflow)?;
            blocked_reasons.insert("direct_quality_unsupported".to_owned());
        }
        if let (Some(context), Some(direct)) = (context_score, direct_score) {
            context_scores.push(context);
            direct_scores.push(direct);
            differences.push(i32::from(context) - i32::from(direct));
        }
    }
    let expected_pairs =
        u32::try_from(pairing_map.pairs.len()).map_err(|_| AblationError::CounterOverflow)?;
    let complete_quality_pairs =
        u32::try_from(differences.len()).map_err(|_| AblationError::CounterOverflow)?;
    let context_mean = mean_u16(&context_scores)?;
    let direct_mean = mean_u16(&direct_scores)?;
    let paired_quality_difference_centipoints = mean_i32(&differences)?;
    let quality_retention_ppm = match (context_mean, direct_mean) {
        (Some(context), Some(direct)) if direct > 0 => Some(
            u32::try_from(
                u64::from(context)
                    .checked_mul(1_000_000)
                    .ok_or(AblationError::CounterOverflow)?
                    / u64::from(direct),
            )
            .map_err(|_| AblationError::CounterOverflow)?,
        ),
        (Some(_), Some(0)) => {
            blocked_reasons.insert("zero_direct_quality".to_owned());
            None
        }
        _ => None,
    };
    let quality_loss_centipoints = match (context_mean, direct_mean) {
        (Some(context), Some(direct)) => Some(direct.saturating_sub(context)),
        _ => None,
    };
    let sensitivity = sensitivity(
        &differences,
        expected_pairs,
        context_ungraded,
        direct_ungraded,
    )?;
    let uncertainty = if differences.is_empty() {
        None
    } else {
        Some(bootstrap_interval(
            &differences,
            protocol.randomization_seed,
        )?)
    };
    let variants = variant_aggregates(
        protocol,
        candidates,
        pairing_map,
        &grades_by_id,
        rubric_evidence,
    )?;
    let efficiency_alongside_quality = match (context_mean, direct_mean) {
        (Some(context_quality), Some(direct_quality))
            if complete_quality_pairs == expected_pairs =>
        {
            efficiency_with_quality(candidates, pairing_map, context_quality, direct_quality)?
        }
        _ => None,
    };
    let context_variant = variants
        .iter()
        .find(|variant| variant.variant == AblationVariant::ContextPack)
        .ok_or(AblationError::AggregateMismatch)?;
    let direct_variant = variants
        .iter()
        .find(|variant| variant.variant == AblationVariant::DirectSequence)
        .ok_or(AblationError::AggregateMismatch)?;
    if complete_quality_pairs != expected_pairs {
        blocked_reasons.insert("incomplete_quality_denominator".to_owned());
    }
    if context_variant.unsupported_claim_rate_ppm.is_none() {
        blocked_reasons.insert("context_unsupported_claim_rate_unavailable".to_owned());
    }
    if direct_variant.unsupported_claim_rate_ppm.is_none() {
        blocked_reasons.insert("direct_unsupported_claim_rate_unavailable".to_owned());
    }
    let decision = if !blocked_reasons.is_empty() {
        AblationDecision::Blocked {
            reason_codes: blocked_reasons.into_iter().collect(),
        }
    } else {
        let mut fallback_reasons = BTreeSet::new();
        if quality_loss_centipoints.is_none_or(|loss| loss > MAX_QUALITY_LOSS_CENTIPOINTS) {
            fallback_reasons.insert("quality_loss_exceeds_two_points".to_owned());
        }
        if context_variant.unsupported_claim_rate_ppm > direct_variant.unsupported_claim_rate_ppm {
            fallback_reasons.insert("unsupported_claim_rate_regressed".to_owned());
        }
        if efficiency_alongside_quality.is_none_or(|efficiency| {
            efficiency.context_calls >= efficiency.direct_calls
                && efficiency.context_tokens >= efficiency.direct_tokens
        }) {
            fallback_reasons.insert("no_efficiency_gain".to_owned());
        }
        if fallback_reasons.is_empty() {
            AblationDecision::Pass
        } else {
            AblationDecision::Fallback {
                reason_codes: fallback_reasons.into_iter().collect(),
            }
        }
    };
    Ok(AblationAggregateReport {
        expected_pairs,
        complete_quality_pairs,
        paired_quality_difference_centipoints,
        quality_retention_ppm,
        quality_loss_centipoints,
        maximum_quality_loss_centipoints: MAX_QUALITY_LOSS_CENTIPOINTS,
        uncertainty,
        sensitivity,
        variants,
        efficiency_alongside_quality,
        decision,
    })
}

fn primary_quality_score(grade: &CandidateGrade) -> Option<u16> {
    match grade {
        CandidateGrade::Scored { score_centipoints } => Some(*score_centipoints),
        CandidateGrade::Failed
        | CandidateGrade::TimedOut
        | CandidateGrade::Cancelled
        | CandidateGrade::UnsupportedExecution => Some(0),
        CandidateGrade::Unsupported { .. }
        | CandidateGrade::NotAvailable
        | CandidateGrade::Excluded => None,
    }
}

fn sensitivity(
    observed: &[i32],
    expected_pairs: u32,
    context_ungraded: u32,
    direct_ungraded: u32,
) -> Result<QualitySensitivity, AblationError> {
    let observed_sum = observed.iter().try_fold(0_i64, |sum, value| {
        sum.checked_add(i64::from(*value))
            .ok_or(AblationError::CounterOverflow)
    })?;
    let missing_pairs = expected_pairs
        .saturating_sub(u32::try_from(observed.len()).map_err(|_| AblationError::CounterOverflow)?);
    let scale = i64::from(MAX_QUALITY_SCORE_CENTIPOINTS);
    let denominator = i64::from(expected_pairs);
    let worst = if denominator == 0 {
        0
    } else {
        observed_sum
            .checked_sub(i64::from(missing_pairs) * scale)
            .ok_or(AblationError::CounterOverflow)?
            / denominator
    };
    let best = if denominator == 0 {
        0
    } else {
        observed_sum
            .checked_add(i64::from(missing_pairs) * scale)
            .ok_or(AblationError::CounterOverflow)?
            / denominator
    };
    Ok(QualitySensitivity {
        observed_difference_centipoints: mean_i32(observed)?,
        worst_case_difference_centipoints: i32::try_from(worst)
            .map_err(|_| AblationError::CounterOverflow)?,
        best_case_difference_centipoints: i32::try_from(best)
            .map_err(|_| AblationError::CounterOverflow)?,
        context_ungraded,
        direct_ungraded,
    })
}

fn bootstrap_interval(
    differences: &[i32],
    seed: u64,
) -> Result<PairedUncertaintyInterval, AblationError> {
    let mut means = Vec::with_capacity(PAIRED_BOOTSTRAP_REPLICATES);
    for replicate in 0..PAIRED_BOOTSTRAP_REPLICATES {
        let mut sum = 0_i64;
        for draw in 0..differences.len() {
            let mut hasher = Sha256::new();
            hasher.update(b"rootlight.ablation.bootstrap.v1");
            hasher.update(seed.to_le_bytes());
            hasher.update(
                u64::try_from(replicate)
                    .map_err(|_| AblationError::CounterOverflow)?
                    .to_le_bytes(),
            );
            hasher.update(
                u64::try_from(draw)
                    .map_err(|_| AblationError::CounterOverflow)?
                    .to_le_bytes(),
            );
            let digest = hasher.finalize();
            let prefix = digest
                .get(..8)
                .ok_or(AblationError::DigestMismatch)?
                .try_into()
                .map_err(|_| AblationError::DigestMismatch)?;
            let index = usize::try_from(u64::from_le_bytes(prefix))
                .map_err(|_| AblationError::CounterOverflow)?
                % differences.len();
            sum = sum
                .checked_add(i64::from(
                    *differences
                        .get(index)
                        .ok_or(AblationError::AggregateMismatch)?,
                ))
                .ok_or(AblationError::CounterOverflow)?;
        }
        means.push(
            i32::try_from(
                sum / i64::try_from(differences.len())
                    .map_err(|_| AblationError::CounterOverflow)?,
            )
            .map_err(|_| AblationError::CounterOverflow)?,
        );
    }
    means.sort_unstable();
    let lower_index = PAIRED_BOOTSTRAP_REPLICATES * 25 / 1_000;
    let upper_index = PAIRED_BOOTSTRAP_REPLICATES * 975 / 1_000;
    Ok(PairedUncertaintyInterval {
        method: UncertaintyMethod::DeterministicPairedBootstrap,
        confidence_ppm: 950_000,
        lower_centipoints: *means
            .get(lower_index)
            .ok_or(AblationError::AggregateMismatch)?,
        upper_centipoints: *means
            .get(upper_index.min(means.len().saturating_sub(1)))
            .ok_or(AblationError::AggregateMismatch)?,
        paired_observations: u32::try_from(differences.len())
            .map_err(|_| AblationError::CounterOverflow)?,
        replicates: u32::try_from(PAIRED_BOOTSTRAP_REPLICATES)
            .map_err(|_| AblationError::CounterOverflow)?,
    })
}

fn variant_aggregates(
    protocol: &AblationProtocol,
    candidates: &[BlindedAblationCandidate],
    pairing_map: &RestrictedPairingMap,
    grades: &BTreeMap<&str, &FinalAutomatedGrade>,
    rubric_evidence: &[CandidateRubricEvidence],
) -> Result<Vec<VariantAggregate>, AblationError> {
    let candidates_by_id = candidates
        .iter()
        .map(|candidate| (candidate.blind_id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    let expected_attempts =
        u32::try_from(protocol.attempt_seeds.len()).map_err(|_| AblationError::CounterOverflow)?;
    let evidence_by_id = rubric_evidence
        .iter()
        .map(|evidence| (evidence.blind_id.as_str(), evidence))
        .collect::<BTreeMap<_, _>>();
    AblationVariant::ALL
        .into_iter()
        .map(|variant| {
            let entries = pairing_map
                .entries
                .iter()
                .filter(|entry| entry.variant == variant)
                .collect::<Vec<_>>();
            let mut aggregate = VariantAggregate {
                variant,
                expected_attempts,
                observed_attempts: u32::try_from(entries.len())
                    .map_err(|_| AblationError::CounterOverflow)?,
                succeeded: 0,
                task_success_rate_ppm: None,
                failed: 0,
                timed_out: 0,
                cancelled: 0,
                unsupported: 0,
                not_available: 0,
                excluded: 0,
                quality_graded: 0,
                mean_quality_centipoints: None,
                attempts_with_unsupported_claims: 0,
                unsupported_claim_assessed: 0,
                unsupported_claim_rate_ppm: None,
                unsupported_claim_categories: BTreeMap::new(),
                resource_totals: BlindedCandidateMetrics::default(),
            };
            let mut quality_scores = Vec::new();
            for entry in entries {
                let candidate = candidates_by_id
                    .get(entry.blind_id.as_str())
                    .ok_or(AblationError::PairingMismatch)?;
                increment_outcome(&mut aggregate, candidate.outcome)?;
                aggregate.resource_totals.calls = aggregate
                    .resource_totals
                    .calls
                    .checked_add(candidate.metrics.calls)
                    .ok_or(AblationError::CounterOverflow)?;
                aggregate.resource_totals.tokens = aggregate
                    .resource_totals
                    .tokens
                    .checked_add(candidate.metrics.tokens)
                    .ok_or(AblationError::CounterOverflow)?;
                aggregate.resource_totals.source_tokens = aggregate
                    .resource_totals
                    .source_tokens
                    .checked_add(candidate.metrics.source_tokens)
                    .ok_or(AblationError::CounterOverflow)?;
                aggregate.resource_totals.elapsed_ns = aggregate
                    .resource_totals
                    .elapsed_ns
                    .checked_add(candidate.metrics.elapsed_ns)
                    .ok_or(AblationError::CounterOverflow)?;
                aggregate.resource_totals.unsupported_claims = aggregate
                    .resource_totals
                    .unsupported_claims
                    .checked_add(candidate.metrics.unsupported_claims)
                    .ok_or(AblationError::CounterOverflow)?;
                if let Some(CandidateRubricEvidence {
                    unsupported_claims: UnsupportedClaimAssessment::Assessed { categories },
                    ..
                }) = evidence_by_id.get(entry.blind_id.as_str()).copied()
                {
                    aggregate.unsupported_claim_assessed = aggregate
                        .unsupported_claim_assessed
                        .checked_add(1)
                        .ok_or(AblationError::CounterOverflow)?;
                    let mut attempt_total = 0_u32;
                    for (category, count) in categories {
                        attempt_total = attempt_total
                            .checked_add(*count)
                            .ok_or(AblationError::CounterOverflow)?;
                        let retained = aggregate
                            .unsupported_claim_categories
                            .entry(*category)
                            .or_insert(0);
                        *retained = retained
                            .checked_add(*count)
                            .ok_or(AblationError::CounterOverflow)?;
                    }
                    if attempt_total > 0 {
                        aggregate.attempts_with_unsupported_claims = aggregate
                            .attempts_with_unsupported_claims
                            .checked_add(1)
                            .ok_or(AblationError::CounterOverflow)?;
                    }
                }
                if let Some(FinalAutomatedGrade {
                    overall: CandidateGrade::Scored { score_centipoints },
                    ..
                }) = grades.get(entry.blind_id.as_str()).copied()
                {
                    quality_scores.push(*score_centipoints);
                }
            }
            aggregate.quality_graded =
                u32::try_from(quality_scores.len()).map_err(|_| AblationError::CounterOverflow)?;
            aggregate.mean_quality_centipoints = mean_u16(&quality_scores)?;
            aggregate.task_success_rate_ppm = if aggregate.observed_attempts == 0 {
                None
            } else {
                Some(
                    u32::try_from(
                        u64::from(aggregate.succeeded)
                            .checked_mul(1_000_000)
                            .ok_or(AblationError::CounterOverflow)?
                            / u64::from(aggregate.observed_attempts),
                    )
                    .map_err(|_| AblationError::CounterOverflow)?,
                )
            };
            aggregate.unsupported_claim_rate_ppm = if aggregate.unsupported_claim_assessed == 0 {
                None
            } else {
                Some(
                    u32::try_from(
                        u64::from(aggregate.attempts_with_unsupported_claims)
                            .checked_mul(1_000_000)
                            .ok_or(AblationError::CounterOverflow)?
                            / u64::from(aggregate.unsupported_claim_assessed),
                    )
                    .map_err(|_| AblationError::CounterOverflow)?,
                )
            };
            Ok(aggregate)
        })
        .collect()
}

fn increment_outcome(
    aggregate: &mut VariantAggregate,
    outcome: BlindedRunOutcome,
) -> Result<(), AblationError> {
    let counter = match outcome {
        BlindedRunOutcome::Succeeded => &mut aggregate.succeeded,
        BlindedRunOutcome::Failed => &mut aggregate.failed,
        BlindedRunOutcome::TimedOut => &mut aggregate.timed_out,
        BlindedRunOutcome::Cancelled => &mut aggregate.cancelled,
        BlindedRunOutcome::Unsupported => &mut aggregate.unsupported,
        BlindedRunOutcome::NotAvailable => &mut aggregate.not_available,
        BlindedRunOutcome::Excluded => &mut aggregate.excluded,
    };
    *counter = counter
        .checked_add(1)
        .ok_or(AblationError::CounterOverflow)?;
    Ok(())
}

fn efficiency_with_quality(
    candidates: &[BlindedAblationCandidate],
    pairing_map: &RestrictedPairingMap,
    context_quality_centipoints: u16,
    direct_quality_centipoints: u16,
) -> Result<Option<EfficiencyAlongsideQuality>, AblationError> {
    let candidates_by_id = candidates
        .iter()
        .map(|candidate| (candidate.blind_id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    let metrics_for = |variant| {
        pairing_map
            .entries
            .iter()
            .filter(|entry| entry.variant == variant)
            .map(|entry| {
                candidates_by_id
                    .get(entry.blind_id.as_str())
                    .map(|candidate| candidate.metrics)
                    .ok_or(AblationError::PairingMismatch)
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let context = metrics_for(AblationVariant::ContextPack)?;
    let direct = metrics_for(AblationVariant::DirectSequence)?;
    if context.is_empty() || context.len() != direct.len() {
        return Ok(None);
    }
    let mean_metrics = |values: &[BlindedCandidateMetrics]| {
        let total =
            values
                .iter()
                .try_fold(BlindedCandidateMetrics::default(), |mut total, value| {
                    total.calls = total
                        .calls
                        .checked_add(value.calls)
                        .ok_or(AblationError::CounterOverflow)?;
                    total.tokens = total
                        .tokens
                        .checked_add(value.tokens)
                        .ok_or(AblationError::CounterOverflow)?;
                    total.source_tokens = total
                        .source_tokens
                        .checked_add(value.source_tokens)
                        .ok_or(AblationError::CounterOverflow)?;
                    total.elapsed_ns = total
                        .elapsed_ns
                        .checked_add(value.elapsed_ns)
                        .ok_or(AblationError::CounterOverflow)?;
                    Ok::<_, AblationError>(total)
                })?;
        let count = u64::try_from(values.len()).map_err(|_| AblationError::CounterOverflow)?;
        Ok::<_, AblationError>(BlindedCandidateMetrics {
            calls: total.calls / count,
            tokens: total.tokens / count,
            source_tokens: total.source_tokens / count,
            elapsed_ns: total.elapsed_ns / count,
            unsupported_claims: 0,
        })
    };
    let context = mean_metrics(&context)?;
    let direct = mean_metrics(&direct)?;
    Ok(Some(EfficiencyAlongsideQuality {
        context_calls: context.calls,
        direct_calls: direct.calls,
        context_tokens: context.tokens,
        direct_tokens: direct.tokens,
        context_source_tokens: context.source_tokens,
        direct_source_tokens: direct.source_tokens,
        context_elapsed_ns: context.elapsed_ns,
        direct_elapsed_ns: direct.elapsed_ns,
        context_quality_centipoints,
        direct_quality_centipoints,
    }))
}

fn mean_u16(values: &[u16]) -> Result<Option<u16>, AblationError> {
    if values.is_empty() {
        return Ok(None);
    }
    let sum = values.iter().try_fold(0_u64, |sum, value| {
        sum.checked_add(u64::from(*value))
            .ok_or(AblationError::CounterOverflow)
    })?;
    Ok(Some(
        u16::try_from(
            sum / u64::try_from(values.len()).map_err(|_| AblationError::CounterOverflow)?,
        )
        .map_err(|_| AblationError::CounterOverflow)?,
    ))
}

fn mean_i32(values: &[i32]) -> Result<Option<i32>, AblationError> {
    if values.is_empty() {
        return Ok(None);
    }
    let sum = values.iter().try_fold(0_i64, |sum, value| {
        sum.checked_add(i64::from(*value))
            .ok_or(AblationError::CounterOverflow)
    })?;
    Ok(Some(
        i32::try_from(
            sum / i64::try_from(values.len()).map_err(|_| AblationError::CounterOverflow)?,
        )
        .map_err(|_| AblationError::CounterOverflow)?,
    ))
}

fn randomized_order_key(seed: u64, blinding_key: &AblationBlindingKey, blind_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"rootlight.ablation.random-order.v1");
    hasher.update(seed.to_le_bytes());
    hasher.update(blinding_key.0);
    hasher.update(blind_id.as_bytes());
    hasher.finalize().into()
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn opaque_id(domain: &str, key: &AblationBlindingKey, parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"rootlight.ablation.opaque-id.v1");
    hasher.update(
        u64::try_from(domain.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(domain.as_bytes());
    hasher.update(key.0);
    for part in parts {
        hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(part);
    }
    format!("{domain}-{}", sha256_hex(&hasher.finalize()))
}

fn digest_json<T: Serialize + ?Sized>(domain: &str, value: &T) -> Result<String, AblationError> {
    let encoded = serde_json::to_vec(value).map_err(|_| AblationError::Serialization)?;
    let mut hasher = Sha256::new();
    hasher.update(
        u64::try_from(domain.len())
            .map_err(|_| AblationError::CounterOverflow)?
            .to_le_bytes(),
    );
    hasher.update(domain.as_bytes());
    hasher.update(
        u64::try_from(encoded.len())
            .map_err(|_| AblationError::CounterOverflow)?
            .to_le_bytes(),
    );
    hasher.update(encoded);
    Ok(sha256_hex(&hasher.finalize()))
}

fn validate_sorted_labels(values: &[String]) -> Result<(), AblationError> {
    if values.is_empty() || values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(AblationError::InvalidLabel);
    }
    values.iter().try_for_each(|value| validate_label(value))
}

fn validate_label(value: &str) -> Result<(), AblationError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(AblationError::InvalidLabel);
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), AblationError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(AblationError::InvalidDigest);
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), AblationError> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(AblationError::InvalidRevision);
    }
    Ok(())
}

/// Context-pack ablation protocol, blinding, grading, or aggregate failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AblationError {
    /// Protocol or evidence schema is unsupported.
    #[error("unsupported context-pack ablation schema")]
    UnsupportedSchema,
    /// Preregistered protocol is malformed or noncanonical.
    #[error("context-pack ablation protocol is invalid")]
    InvalidProtocol,
    /// A normalized identifier is invalid.
    #[error("context-pack ablation label is invalid")]
    InvalidLabel,
    /// A digest is not canonical lowercase SHA-256.
    #[error("context-pack ablation digest is invalid")]
    InvalidDigest,
    /// Source revision is not canonical lowercase Git identity.
    #[error("context-pack ablation source revision is invalid")]
    InvalidRevision,
    /// The package does not contain the required context-pack task.
    #[error("context-pack task is absent from trajectory evidence")]
    MissingContextTask,
    /// Protocol inputs differ from the immutable trajectory package.
    #[error("context-pack ablation protocol binding differs")]
    ProtocolBindingMismatch,
    /// One source attempt matched more than one representation of a variant.
    #[error("context-pack ablation variant selection is ambiguous")]
    AmbiguousVariant,
    /// Opaque candidate identities collided.
    #[error("context-pack ablation blind identity collided")]
    BlindIdCollision,
    /// Candidate and restricted pairing records do not reconcile.
    #[error("context-pack ablation pairing does not reconcile")]
    PairingMismatch,
    /// Candidate or evidence digest differs from recomputation.
    #[error("context-pack ablation digest does not reconcile")]
    DigestMismatch,
    /// Actual token accounting is absent.
    #[error("context-pack ablation requires actual token counts")]
    MissingActualTokens,
    /// A measured direct-sequence candidate violates its preregistration.
    #[error("direct-sequence measurement violates the preregistered contract")]
    InvalidDirectMeasurement,
    /// Rubric evidence is incomplete, unbounded, or bound to another candidate.
    #[error("context-pack rubric evidence is invalid")]
    InvalidRubricEvidence,
    /// More than one rubric record targets the same opaque candidate.
    #[error("context-pack rubric evidence contains a duplicate candidate")]
    DuplicateRubricEvidence,
    /// Rubric evidence targets an unknown opaque candidate.
    #[error("context-pack rubric evidence targets an unknown candidate")]
    UnknownBlindId,
    /// A raw or final automated grade is malformed.
    #[error("context-pack automated grade is invalid")]
    InvalidGrade,
    /// Two unsupported grading reasons cannot be adjudicated truthfully.
    #[error("context-pack automated graders reported conflicting unsupported reasons")]
    ConflictingUnsupportedReasons,
    /// Automated adjudication exceeded the preregistered per-candidate bound.
    #[error("context-pack automated adjudication exceeded its bound")]
    AdjudicationLimitExceeded,
    /// Raw grades or adjudications differ from deterministic recomputation.
    #[error("context-pack automated grades do not reconcile")]
    GradeReconciliationMismatch,
    /// Aggregate values differ from deterministic recomputation.
    #[error("context-pack ablation aggregate does not reconcile")]
    AggregateMismatch,
    /// Evidence serialization failed.
    #[error("context-pack ablation serialization failed")]
    Serialization,
    /// Encoded evidence is malformed or has unknown fields.
    #[error("context-pack ablation encoding is invalid")]
    InvalidEncoding,
    /// Encoded evidence exceeds its hard byte limit.
    #[error("context-pack ablation evidence exceeds its byte limit")]
    PackageTooLarge,
    /// Counter arithmetic overflowed.
    #[error("context-pack ablation counter overflow")]
    CounterOverflow,
    /// Source trajectory evidence is invalid.
    #[error(transparent)]
    Trajectory(#[from] crate::TrajectoryError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActualTokenizerIdentity, RawTrajectoryAttempt, RawTrajectoryCall, TrajectoryAdapter,
        TrajectoryClaimSignals, TrajectoryExecutionBoundary, TrajectoryExecutionInput,
        TrajectoryExposureProfile, TrajectoryOperationStatus, TrajectoryTokenizer,
        TrajectoryToolIdentity, UnavailableTrajectoryAdapter, preregistered_trajectory_protocol,
        run_trajectory_suite,
    };

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

        fn count(&self, input: &[u8]) -> Result<u64, crate::TrajectoryError> {
            std::str::from_utf8(input).map_err(|_| crate::TrajectoryError::InvalidUtf8)?;
            u64::try_from(input.len()).map_err(|_| crate::TrajectoryError::CounterOverflow)
        }
    }

    struct SuccessfulAdapter {
        condition: TrajectoryCondition,
        boundary: TrajectoryExecutionBoundary,
    }

    impl TrajectoryAdapter for SuccessfulAdapter {
        fn condition(&self) -> TrajectoryCondition {
            self.condition
        }

        fn execution_boundary(&self) -> TrajectoryExecutionBoundary {
            self.boundary
        }

        fn execute(&mut self, input: TrajectoryExecutionInput<'_>) -> RawTrajectoryAttempt {
            let tool_ids = match self.condition {
                TrajectoryCondition::Rootlight => input.workflow.rootlight_tools.clone(),
                TrajectoryCondition::BoundedFileExploration => {
                    vec!["bounded_file.explore".to_owned()]
                }
                TrajectoryCondition::CodebaseMemory => {
                    vec!["codebase_memory_process_v1".to_owned()]
                }
            };
            RawTrajectoryAttempt {
                outcome: TrajectoryAttemptOutcome::Succeeded,
                calls: tool_ids
                    .into_iter()
                    .enumerate()
                    .map(|(index, tool_id)| RawTrajectoryCall {
                        operation_id: format!("operation-{index:02}"),
                        tool: TrajectoryToolIdentity {
                            tool_id,
                            tool_version: "v1".to_owned(),
                        },
                        exposure_profile: TrajectoryExposureProfile::Analysis,
                        operation_status: TrajectoryOperationStatus::Succeeded,
                        retry_ordinal: 0,
                        request_frame: br#"{"request":"fixture"}"#.to_vec(),
                        response_frame: br#"{"response":"fixture"}"#.to_vec(),
                        source_frame: b"fixture".to_vec(),
                        elapsed_ns: 10,
                        result_items: 1,
                        truncated: false,
                        continuation_available: false,
                        claim_signals: TrajectoryClaimSignals::default(),
                    })
                    .collect(),
            }
        }
    }

    fn trajectory_package() -> TrajectoryEvidencePackage {
        let protocol =
            preregistered_trajectory_protocol("ab".repeat(32)).expect("protocol is valid");
        let mut rootlight = SuccessfulAdapter {
            condition: TrajectoryCondition::Rootlight,
            boundary: TrajectoryExecutionBoundary::DaemonMcpProcess,
        };
        let mut codebase = UnavailableTrajectoryAdapter::new(
            TrajectoryCondition::CodebaseMemory,
            "codebase_memory_process_v1",
            "executable_not_available",
        )
        .expect("unavailable baseline is valid");
        let mut bounded = SuccessfulAdapter {
            condition: TrajectoryCondition::BoundedFileExploration,
            boundary: TrajectoryExecutionBoundary::LocalBoundedFiles,
        };
        run_trajectory_suite(
            protocol,
            &mut rootlight,
            &mut codebase,
            &mut bounded,
            &ByteTokenizer,
        )
        .expect("84-attempt package is valid")
    }

    fn blinding_key() -> AblationBlindingKey {
        AblationBlindingKey::new([0x5a; 32])
    }

    fn protocol(package: &TrajectoryEvidencePackage) -> AblationProtocol {
        preregister_context_pack_ablation(package, &blinding_key(), &"12".repeat(20))
            .expect("ablation protocol is valid")
    }

    fn all_checks(
        candidate: &BlindedAblationCandidate,
        checks: Vec<bool>,
    ) -> CandidateRubricEvidence {
        CandidateRubricEvidence {
            blind_id: candidate.blind_id.clone(),
            candidate_sha256: candidate.candidate_sha256.clone(),
            observations: RubricDimension::ALL
                .into_iter()
                .map(|dimension| {
                    (
                        dimension,
                        RubricObservation::Checks {
                            checks: checks.clone(),
                        },
                    )
                })
                .collect(),
            unsupported_claims: UnsupportedClaimAssessment::Assessed {
                categories: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn real_84_attempt_package_blocks_without_direct_or_answer_evidence() {
        let package = trajectory_package();
        assert_eq!(package.denominator.expected_attempts, 84);
        let evidence =
            evaluate_context_pack_ablation(&package, protocol(&package), &blinding_key(), vec![])
                .expect("honest blocked evidence is produced");
        assert_eq!(evidence.aggregate.expected_pairs, 2);
        assert_eq!(evidence.aggregate.complete_quality_pairs, 0);
        assert!(matches!(
            &evidence.aggregate.decision,
            AblationDecision::Blocked { reason_codes }
                if reason_codes == &[
                    "context_quality_unsupported".to_owned(),
                    "context_unsupported_claim_rate_unavailable".to_owned(),
                    "direct_unsupported_claim_rate_unavailable".to_owned(),
                    "incomplete_quality_denominator".to_owned(),
                    "missing_direct_sequence".to_owned()
                ]
        ));
        assert!(evidence.aggregate.efficiency_alongside_quality.is_none());
        let context = evidence
            .aggregate
            .variants
            .iter()
            .find(|variant| variant.variant == AblationVariant::ContextPack)
            .expect("context aggregate exists");
        assert_eq!(context.observed_attempts, 2);
        assert_eq!(context.quality_graded, 0);
        assert_eq!(context.unsupported_claim_rate_ppm, None);
    }

    #[test]
    fn measured_direct_sequences_complete_preregistered_primary_pairs() {
        let package = trajectory_package();
        let ablation_protocol = protocol(&package);
        let key = blinding_key();
        let mut prepared = prepare_blinded_ablation(&package, &ablation_protocol, &key)
            .expect("base preparation succeeds");
        let tools = vec![
            "code.locate".to_owned(),
            "symbol.explain".to_owned(),
            "source.read".to_owned(),
            "symbol.relationships".to_owned(),
        ];
        for attempt_index in 0..2 {
            prepared
                .add_direct_sequence_measurement(
                    &ablation_protocol,
                    &key,
                    attempt_index,
                    BlindedRunOutcome::Succeeded,
                    BlindedCandidateMetrics {
                        calls: 4,
                        tokens: 400,
                        source_tokens: 100,
                        elapsed_ns: 4_000,
                        unsupported_claims: 0,
                    },
                    &tools,
                )
                .expect("direct measurement completes its pair");
        }
        let rubric_evidence = prepared
            .candidates
            .iter()
            .map(|candidate| all_checks(candidate, vec![true, true]))
            .collect();
        let evidence = prepared
            .evaluate(ablation_protocol, rubric_evidence)
            .expect("complete measured pairs evaluate");
        assert_eq!(evidence.aggregate.expected_pairs, 2);
        assert_eq!(evidence.aggregate.complete_quality_pairs, 2);
        assert_eq!(evidence.aggregate.quality_retention_ppm, Some(1_000_000));
        assert!(evidence.aggregate.uncertainty.is_some());
        assert!(matches!(
            evidence.aggregate.decision,
            AblationDecision::Pass
        ));

        let ablation_protocol = protocol(&package);
        let mut invalid = prepare_blinded_ablation(&package, &ablation_protocol, &key)
            .expect("second preparation succeeds");
        assert!(matches!(
            invalid.add_direct_sequence_measurement(
                &ablation_protocol,
                &key,
                0,
                BlindedRunOutcome::Succeeded,
                BlindedCandidateMetrics {
                    calls: 1,
                    tokens: 1,
                    source_tokens: 0,
                    elapsed_ns: 1,
                    unsupported_claims: 0,
                },
                &["code.locate".to_owned()],
            ),
            Err(AblationError::InvalidDirectMeasurement)
        ));
    }

    #[test]
    fn graders_are_explicitly_automated_independent_and_bounded() {
        let package = trajectory_package();
        let prepared = prepare_blinded_ablation(&package, &protocol(&package), &blinding_key())
            .expect("candidates prepare");
        let candidate = prepared
            .candidates
            .iter()
            .find(|candidate| candidate.outcome == BlindedRunOutcome::Succeeded)
            .expect("successful candidate exists");
        let (raw, adjudications, final_grades, agreement) = grade_candidates(
            std::slice::from_ref(candidate),
            &[all_checks(candidate, vec![true, false])],
        )
        .expect("automated grades are produced");
        assert_eq!(raw.len(), 2);
        assert!(raw.iter().all(|grade| {
            grade.grader.kind == GraderKind::Automated
                && grade.grader.grader_id.starts_with("automated-")
        }));
        assert_eq!(agreement.dimension_comparisons, 7);
        assert_eq!(agreement.exact_dimension_agreements, 0);
        assert_eq!(adjudications.len(), 7);
        assert!(adjudications.iter().all(|record| {
            record.adjudicator.kind == GraderKind::Automated
                && record.adjudicator.grader_id == AUTOMATED_ADJUDICATOR_ID
                && record.resolved
                    == DimensionGrade::Scored {
                        score_centipoints: 0,
                    }
        }));
        assert_eq!(
            final_grades[0].overall,
            CandidateGrade::Scored {
                score_centipoints: 0
            }
        );
    }

    #[test]
    fn blind_ids_are_randomized_condition_free_and_key_bound() {
        let package = trajectory_package();
        let protocol = protocol(&package);
        let first = prepare_blinded_ablation(&package, &protocol, &blinding_key())
            .expect("first preparation succeeds");
        let second = prepare_blinded_ablation(&package, &protocol, &blinding_key())
            .expect("second preparation succeeds");
        assert_eq!(first, second);
        let encoded =
            serde_json::to_string(&first.candidates).expect("blinded candidates serialize");
        for forbidden in [
            "rootlight",
            "codebase_memory",
            "bounded_file",
            "context.pack",
            "bug-fix-context",
        ] {
            assert!(!encoded.contains(forbidden));
        }
        assert!(
            first
                .pairing_map
                .entries
                .iter()
                .any(|entry| entry.variant == AblationVariant::ContextPack)
        );

        let different_key = AblationBlindingKey::new([0xa5; 32]);
        let different_protocol =
            preregister_context_pack_ablation(&package, &different_key, &"12".repeat(20))
                .expect("second key protocol is valid");
        let different = prepare_blinded_ablation(&package, &different_protocol, &different_key)
            .expect("second key preparation succeeds");
        assert_ne!(
            first
                .candidates
                .iter()
                .map(|candidate| &candidate.blind_id)
                .collect::<Vec<_>>(),
            different
                .candidates
                .iter()
                .map(|candidate| &candidate.blind_id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn evidence_round_trip_and_mutations_recompute_every_layer() {
        let package = trajectory_package();
        let evidence =
            evaluate_context_pack_ablation(&package, protocol(&package), &blinding_key(), vec![])
                .expect("evidence is produced");
        let encoded = encode_context_pack_ablation(&evidence).expect("evidence encodes");
        assert_eq!(
            decode_context_pack_ablation(&encoded).expect("evidence decodes"),
            evidence
        );

        let mut threshold = evidence.clone();
        threshold.protocol.max_quality_loss_centipoints = 201;
        assert!(matches!(
            threshold.validate(),
            Err(AblationError::InvalidProtocol)
        ));

        let mut source_revision = evidence.clone();
        source_revision.protocol.source_revision = "not-a-revision".to_owned();
        assert!(matches!(
            source_revision.validate(),
            Err(AblationError::InvalidRevision)
        ));

        let mut candidate = evidence.clone();
        candidate.blinded_candidates[0].metrics.tokens = candidate.blinded_candidates[0]
            .metrics
            .tokens
            .saturating_add(1);
        assert!(matches!(
            candidate.validate(),
            Err(AblationError::DigestMismatch)
        ));

        let mut raw_grade = evidence.clone();
        raw_grade.raw_automated_grades[0].overall = CandidateGrade::Failed;
        assert!(matches!(
            raw_grade.validate(),
            Err(AblationError::GradeReconciliationMismatch)
        ));

        let mut aggregate = evidence.clone();
        aggregate.aggregate.complete_quality_pairs = 1;
        assert!(matches!(
            aggregate.validate(),
            Err(AblationError::AggregateMismatch)
        ));

        let mut order = evidence.clone();
        order.restricted_pairing_map.entries.swap(0, 1);
        assert!(matches!(
            order.validate(),
            Err(AblationError::PairingMismatch)
        ));

        let mut value: serde_json::Value =
            serde_json::from_slice(&encoded).expect("encoded evidence is JSON");
        value["human_grader"] = serde_json::Value::Bool(true);
        let unknown = serde_json::to_vec(&value).expect("mutated evidence serializes");
        assert!(matches!(
            decode_context_pack_ablation(&unknown),
            Err(AblationError::InvalidEncoding)
        ));
    }

    fn synthetic_complete(
        context_score: u16,
        direct_score: u16,
        context_outcome: BlindedRunOutcome,
    ) -> (
        AblationProtocol,
        Vec<BlindedAblationCandidate>,
        RestrictedPairingMap,
        Vec<FinalAutomatedGrade>,
        Vec<CandidateRubricEvidence>,
    ) {
        let package = trajectory_package();
        let protocol = protocol(&package);
        let mut prepared = prepare_blinded_ablation(&package, &protocol, &blinding_key())
            .expect("base preparation succeeds");
        let context_entries = prepared
            .pairing_map
            .entries
            .iter()
            .filter(|entry| entry.variant == AblationVariant::ContextPack)
            .cloned()
            .collect::<Vec<_>>();
        for (index, context_entry) in context_entries.iter().enumerate() {
            let context_candidate = prepared
                .candidates
                .iter_mut()
                .find(|candidate| candidate.blind_id == context_entry.blind_id)
                .expect("context candidate exists");
            context_candidate.outcome = context_outcome;
            context_candidate.metrics = BlindedCandidateMetrics {
                calls: 1,
                tokens: 100,
                source_tokens: 30,
                elapsed_ns: 1_000,
                unsupported_claims: 0,
            };
            context_candidate.candidate_sha256 = digest_json(
                "rootlight.ablation.candidate.v1",
                &(
                    context_candidate.blind_id.as_str(),
                    context_candidate.pair_id.as_str(),
                    context_candidate.task_sha256.as_str(),
                    context_candidate.outcome,
                    context_candidate.metrics,
                ),
            )
            .expect("context candidate digest");
            let blind_id = format!("candidate-direct-synthetic-{index:02}");
            let pair_id = context_candidate.pair_id.clone();
            let task_sha256 = context_candidate.task_sha256.clone();
            let outcome = BlindedRunOutcome::Succeeded;
            let metrics = BlindedCandidateMetrics {
                calls: 4,
                tokens: 400,
                source_tokens: 100,
                elapsed_ns: 4_000,
                unsupported_claims: 0,
            };
            let candidate_sha256 = digest_json(
                "rootlight.ablation.candidate.v1",
                &(
                    blind_id.as_str(),
                    pair_id.as_str(),
                    task_sha256.as_str(),
                    outcome,
                    metrics,
                ),
            )
            .expect("direct candidate digest");
            prepared.candidates.push(BlindedAblationCandidate {
                blind_id: blind_id.clone(),
                pair_id: pair_id.clone(),
                task_sha256,
                outcome,
                metrics,
                candidate_sha256,
            });
            prepared.pairing_map.entries.push(RestrictedPairingEntry {
                blind_id,
                pair_id: pair_id.clone(),
                attempt_id: format!("synthetic-direct-{index:02}"),
                variant: AblationVariant::DirectSequence,
                order_sha256: "cd".repeat(32),
            });
            let pair = prepared
                .pairing_map
                .pairs
                .iter_mut()
                .find(|pair| pair.pair_id == pair_id)
                .expect("pair exists");
            pair.missing_variants
                .retain(|variant| *variant != AblationVariant::DirectSequence);
        }
        let grades = prepared
            .candidates
            .iter()
            .map(|candidate| {
                let score = prepared
                    .pairing_map
                    .entries
                    .iter()
                    .find(|entry| entry.blind_id == candidate.blind_id)
                    .map_or(CandidateGrade::NotAvailable, |entry| match entry.variant {
                        AblationVariant::ContextPack => match context_outcome {
                            BlindedRunOutcome::Succeeded => CandidateGrade::Scored {
                                score_centipoints: context_score,
                            },
                            BlindedRunOutcome::Failed => CandidateGrade::Failed,
                            _ => CandidateGrade::Excluded,
                        },
                        AblationVariant::DirectSequence => CandidateGrade::Scored {
                            score_centipoints: direct_score,
                        },
                        AblationVariant::CodebaseMemory => CandidateGrade::NotAvailable,
                        AblationVariant::BoundedFileExploration => CandidateGrade::Unsupported {
                            reason_code: "not_primary_pair".to_owned(),
                        },
                    });
                FinalAutomatedGrade {
                    blind_id: candidate.blind_id.clone(),
                    dimensions: BTreeMap::new(),
                    overall: score,
                }
            })
            .collect();
        let rubric_evidence = prepared
            .candidates
            .iter()
            .map(|candidate| all_checks(candidate, vec![true]))
            .collect();
        (
            protocol,
            prepared.candidates,
            prepared.pairing_map,
            grades,
            rubric_evidence,
        )
    }

    #[test]
    fn aggregate_algorithm_distinguishes_pass_fallback_and_failed_runs() {
        let (protocol, candidates, pairing, grades, rubric_evidence) =
            synthetic_complete(9_800, 9_900, BlindedRunOutcome::Succeeded);
        let pass = aggregate_report(&protocol, &candidates, &pairing, &grades, &rubric_evidence)
            .expect("pass aggregates");
        assert_eq!(pass.quality_loss_centipoints, Some(100));
        assert!(matches!(pass.decision, AblationDecision::Pass));
        assert!(pass.efficiency_alongside_quality.is_some());
        assert!(pass.uncertainty.is_some());
        assert!(pass.variants.iter().all(|variant| {
            variant.observed_attempts == 0 || variant.task_success_rate_ppm.is_some()
        }));

        let (protocol, candidates, pairing, grades, rubric_evidence) =
            synthetic_complete(9_600, 9_900, BlindedRunOutcome::Succeeded);
        let fallback =
            aggregate_report(&protocol, &candidates, &pairing, &grades, &rubric_evidence)
                .expect("fallback aggregates");
        assert_eq!(fallback.quality_loss_centipoints, Some(300));
        assert!(matches!(
            fallback.decision,
            AblationDecision::Fallback { ref reason_codes }
                if reason_codes == &["quality_loss_exceeds_two_points".to_owned()]
        ));

        let (protocol, candidates, pairing, grades, rubric_evidence) =
            synthetic_complete(0, 9_900, BlindedRunOutcome::Failed);
        let failed = aggregate_report(&protocol, &candidates, &pairing, &grades, &rubric_evidence)
            .expect("failures aggregate");
        let context = failed
            .variants
            .iter()
            .find(|variant| variant.variant == AblationVariant::ContextPack)
            .expect("context aggregate exists");
        assert_eq!(context.failed, 2);
        assert_eq!(context.task_success_rate_ppm, Some(0));
        assert_eq!(failed.complete_quality_pairs, 2);
        assert_eq!(failed.quality_loss_centipoints, Some(9_900));
        assert!(matches!(failed.decision, AblationDecision::Fallback { .. }));
    }

    #[test]
    fn sensitivity_and_bootstrap_are_reproducible() {
        let first = sensitivity(&[100, -100], 4, 2, 2).expect("sensitivity computes");
        let second = sensitivity(&[100, -100], 4, 2, 2).expect("sensitivity recomputes");
        assert_eq!(first, second);
        assert_eq!(first.observed_difference_centipoints, Some(0));
        assert_eq!(first.worst_case_difference_centipoints, -5_000);
        assert_eq!(first.best_case_difference_centipoints, 5_000);

        let first_interval = bootstrap_interval(&[-100, 100], 17).expect("interval computes");
        let second_interval = bootstrap_interval(&[-100, 100], 17).expect("interval recomputes");
        assert_eq!(first_interval, second_interval);
        assert_eq!(first_interval.paired_observations, 2);
        assert!(first_interval.lower_centipoints <= first_interval.upper_centipoints);
    }

    #[test]
    fn imports_remain_source_free_and_do_not_claim_human_grading() {
        let package = trajectory_package();
        let evidence =
            evaluate_context_pack_ablation(&package, protocol(&package), &blinding_key(), vec![])
                .expect("evidence is produced");
        let encoded = encode_context_pack_ablation(&evidence).expect("evidence encodes");
        let text = std::str::from_utf8(&encoded).expect("evidence is UTF-8");
        assert!(!text.contains("request_frame"));
        assert!(!text.contains("response_frame"));
        assert!(!text.contains("source_frame"));
        assert!(!text.contains("human_grader"));
        assert!(text.contains("\"kind\":\"automated\""));
    }
}
