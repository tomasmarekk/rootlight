//! Typed response-profile shaping for bounded analytical results.
//!
//! Shaping starts from one semantically selected typed result and may only
//! remove approved representation detail. Transport envelopes, recovery data,
//! and hard execution budgets remain owned by their existing layers.

use crate::policy::{BudgetCharge, CancellationSignal, ExecutionContext, ExecutionPolicyError};
use rootlight_ir::SourceRef;
use rootlight_mcp_contract::{
    TrustClassification,
    capability::{ResponseProfileSupport, capability_for},
    catalog::McpTool,
    change::{ChangeImpactData, PlanChangeData, TestCandidate, TestsSelectData},
    completeness::ResultCompleteness,
    context::{BatchTool, ContextPackData},
    intent::{
        ArchitectureCyclesData, ArchitectureOverviewData, CodeDeadData, FlowTraceData,
        SymbolRelationshipsData,
    },
    vertical::{
        CodeLocateData, ReadEnvelope, ResponseProfile, ResponseWarning, SourceReadData,
        SymbolExplainData,
    },
};
use serde_json::Value;

/// Largest rationale prefix accepted by the shared analytical shaper.
pub const MAX_PROFILE_RATIONALE_ITEMS: u16 = 16;
/// Largest source-reference or provenance prefix accepted per result.
pub const MAX_PROFILE_EVIDENCE_REFERENCES: u16 = 16;
/// Largest source-preview allowance accepted by one shaping policy.
pub const MAX_PROFILE_SOURCE_PREVIEW_BYTES: u32 = 524_288;
/// Largest candidate set accepted by the bounded representation selector.
pub const MAX_PROFILE_CANDIDATES: usize = 4_096;
const STANDARD_CONTEXT_SNIPPET_BYTES: usize = 4_096;

/// Source-bearing detail a tool permits its profile shaper to retain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProfileSourceAccess {
    /// The shaped section cannot expose source-bearing detail.
    None,
    /// Immutable source references are permitted, but source bodies are not.
    References,
    /// Immutable source references and bounded source previews are permitted.
    Snippets,
}

/// Optional typed enrichment fields enabled by one profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OptionalProfileFields {
    signatures: bool,
    cost_hints: bool,
    verification_hints: bool,
    optional_metadata: bool,
}

impl OptionalProfileFields {
    /// Creates an explicit optional-field policy.
    #[must_use]
    pub const fn new(
        signatures: bool,
        cost_hints: bool,
        verification_hints: bool,
        optional_metadata: bool,
    ) -> Self {
        Self {
            signatures,
            cost_hints,
            verification_hints,
            optional_metadata,
        }
    }

    /// Reports whether repository-controlled signatures are retained.
    #[must_use]
    pub const fn signatures(self) -> bool {
        self.signatures
    }

    /// Reports whether estimated-cost hints are retained.
    #[must_use]
    pub const fn cost_hints(self) -> bool {
        self.cost_hints
    }

    /// Reports whether source-free verification hints are retained.
    #[must_use]
    pub const fn verification_hints(self) -> bool {
        self.verification_hints
    }

    /// Reports whether other schema-optional explanatory metadata is retained.
    #[must_use]
    pub const fn optional_metadata(self) -> bool {
        self.optional_metadata
    }

    const fn is_subset_of(self, other: Self) -> bool {
        (!self.signatures || other.signatures)
            && (!self.cost_hints || other.cost_hints)
            && (!self.verification_hints || other.verification_hints)
            && (!self.optional_metadata || other.optional_metadata)
    }
}

/// Representation-only ceilings and optional-field policy for one profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileLimits {
    rationale_items_per_result: u16,
    evidence_references_per_result: u16,
    source_preview_bytes: u32,
    source_access: ProfileSourceAccess,
    optional_fields: OptionalProfileFields,
}

impl ProfileLimits {
    /// Creates one bounded profile limit set.
    ///
    /// # Errors
    ///
    /// Returns [`ProfilePolicyError`] when a ceiling exceeds the shared
    /// analytical schema bounds or source permissions disagree with the
    /// requested representation.
    pub const fn new(
        rationale_items_per_result: u16,
        evidence_references_per_result: u16,
        source_preview_bytes: u32,
        source_access: ProfileSourceAccess,
        optional_fields: OptionalProfileFields,
    ) -> Result<Self, ProfilePolicyError> {
        if rationale_items_per_result == 0
            || rationale_items_per_result > MAX_PROFILE_RATIONALE_ITEMS
        {
            return Err(ProfilePolicyError::InvalidRationaleLimit);
        }
        if evidence_references_per_result > MAX_PROFILE_EVIDENCE_REFERENCES {
            return Err(ProfilePolicyError::InvalidEvidenceReferenceLimit);
        }
        if source_preview_bytes > MAX_PROFILE_SOURCE_PREVIEW_BYTES {
            return Err(ProfilePolicyError::InvalidSourcePreviewLimit);
        }
        if evidence_references_per_result > 0 && matches!(source_access, ProfileSourceAccess::None)
        {
            return Err(ProfilePolicyError::EvidenceRequiresSourceAccess);
        }
        if source_preview_bytes > 0 && !matches!(source_access, ProfileSourceAccess::Snippets) {
            return Err(ProfilePolicyError::PreviewRequiresSnippetAccess);
        }
        Ok(Self {
            rationale_items_per_result,
            evidence_references_per_result,
            source_preview_bytes,
            source_access,
            optional_fields,
        })
    }

    /// Returns the rationale prefix retained for each semantic result.
    #[must_use]
    pub const fn rationale_items_per_result(self) -> u16 {
        self.rationale_items_per_result
    }

    /// Returns the evidence-reference prefix retained for each result.
    #[must_use]
    pub const fn evidence_references_per_result(self) -> u16 {
        self.evidence_references_per_result
    }

    /// Returns the aggregate source-preview byte allowance.
    #[must_use]
    pub const fn source_preview_bytes(self) -> u32 {
        self.source_preview_bytes
    }

    /// Returns the permitted source-bearing representation class.
    #[must_use]
    pub const fn source_access(self) -> ProfileSourceAccess {
        self.source_access
    }

    /// Returns the schema-optional fields retained by this profile.
    #[must_use]
    pub const fn optional_fields(self) -> OptionalProfileFields {
        self.optional_fields
    }

    const fn is_subset_of(self, other: Self) -> bool {
        self.rationale_items_per_result <= other.rationale_items_per_result
            && self.evidence_references_per_result <= other.evidence_references_per_result
            && self.source_preview_bytes <= other.source_preview_bytes
            && source_access_rank(self.source_access) <= source_access_rank(other.source_access)
            && self.optional_fields.is_subset_of(other.optional_fields)
    }
}

/// Validated monotone limits for compact, standard, and evidence projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileLimitSet {
    compact: ProfileLimits,
    standard: ProfileLimits,
    evidence: ProfileLimits,
}

impl ProfileLimitSet {
    /// Validates a monotone three-profile representation policy.
    ///
    /// Compact and standard may retain immutable source references but cannot
    /// expose source bodies. Evidence is the only profile allowed to admit
    /// bounded source previews.
    ///
    /// # Errors
    ///
    /// Returns [`ProfilePolicyError`] when a richer profile removes detail
    /// admitted by a narrower profile, or compact/standard admit snippets.
    pub const fn new(
        compact: ProfileLimits,
        standard: ProfileLimits,
        evidence: ProfileLimits,
    ) -> Result<Self, ProfilePolicyError> {
        if compact.source_preview_bytes > 0
            || standard.source_preview_bytes > 0
            || matches!(compact.source_access, ProfileSourceAccess::Snippets)
            || matches!(standard.source_access, ProfileSourceAccess::Snippets)
        {
            return Err(ProfilePolicyError::SourcePreviewOutsideEvidence);
        }
        if !compact.is_subset_of(standard) {
            return Err(ProfilePolicyError::NonMonotoneCompactToStandard);
        }
        if !standard.is_subset_of(evidence) {
            return Err(ProfilePolicyError::NonMonotoneStandardToEvidence);
        }
        Ok(Self {
            compact,
            standard,
            evidence,
        })
    }

    /// Returns the reviewed defaults for current analytical output schemas.
    ///
    /// These schemas expose source references but no optional source-body
    /// fields. Evidence therefore expands reference and rationale cardinality
    /// without inventing a snippet representation.
    #[must_use]
    pub const fn analytical() -> Self {
        Self {
            compact: ProfileLimits {
                rationale_items_per_result: 1,
                evidence_references_per_result: 1,
                source_preview_bytes: 0,
                source_access: ProfileSourceAccess::References,
                optional_fields: OptionalProfileFields::new(false, false, false, false),
            },
            standard: ProfileLimits {
                rationale_items_per_result: 4,
                evidence_references_per_result: 4,
                source_preview_bytes: 0,
                source_access: ProfileSourceAccess::References,
                optional_fields: OptionalProfileFields::new(true, true, true, true),
            },
            evidence: ProfileLimits {
                rationale_items_per_result: MAX_PROFILE_RATIONALE_ITEMS,
                evidence_references_per_result: MAX_PROFILE_EVIDENCE_REFERENCES,
                source_preview_bytes: 0,
                source_access: ProfileSourceAccess::References,
                optional_fields: OptionalProfileFields::new(true, true, true, true),
            },
        }
    }

    /// Returns the validated limits for one canonical profile.
    #[must_use]
    pub const fn limits(self, profile: ResponseProfile) -> ProfileLimits {
        match profile {
            ResponseProfile::Compact => self.compact,
            ResponseProfile::Standard => self.standard,
            ResponseProfile::Evidence => self.evidence,
        }
    }
}

impl Default for ProfileLimitSet {
    fn default() -> Self {
        Self::analytical()
    }
}

/// Invalid response-profile policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProfilePolicyError {
    /// Rationale count is zero or exceeds the shared schema ceiling.
    #[error("response profile rationale limit is invalid")]
    InvalidRationaleLimit,
    /// Evidence-reference count exceeds the shared schema ceiling.
    #[error("response profile evidence-reference limit is invalid")]
    InvalidEvidenceReferenceLimit,
    /// Source-preview bytes exceed the shared response-budget ceiling.
    #[error("response profile source-preview limit is invalid")]
    InvalidSourcePreviewLimit,
    /// A policy admitted evidence references while prohibiting source access.
    #[error("response profile evidence requires source-reference access")]
    EvidenceRequiresSourceAccess,
    /// A positive preview allowance omitted snippet authorization.
    #[error("response profile preview requires snippet access")]
    PreviewRequiresSnippetAccess,
    /// Compact or standard attempted to expose source bodies.
    #[error("source previews are available only in the evidence profile")]
    SourcePreviewOutsideEvidence,
    /// Standard removed representation detail available in compact.
    #[error("standard response profile is narrower than compact")]
    NonMonotoneCompactToStandard,
    /// Evidence removed representation detail available in standard.
    #[error("evidence response profile is narrower than standard")]
    NonMonotoneStandardToEvidence,
}

/// Kind of optional representation candidate considered by the selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileCandidateKind {
    /// Source-free rationale or explanatory metadata.
    Rationale,
    /// Immutable source reference or bounded provenance record.
    EvidenceReference,
    /// Repository-controlled source preview.
    SourcePreview,
}

/// Cost and class of one already-authorized optional representation item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileCandidate {
    kind: ProfileCandidateKind,
    estimated_tokens: u64,
    source_bytes: u64,
}

impl ProfileCandidate {
    /// Creates one source-free rationale candidate.
    #[must_use]
    pub const fn rationale(estimated_tokens: u64) -> Self {
        Self {
            kind: ProfileCandidateKind::Rationale,
            estimated_tokens,
            source_bytes: 0,
        }
    }

    /// Creates one immutable source-reference or provenance candidate.
    #[must_use]
    pub const fn evidence_reference(estimated_tokens: u64) -> Self {
        Self {
            kind: ProfileCandidateKind::EvidenceReference,
            estimated_tokens,
            source_bytes: 0,
        }
    }

    /// Creates one bounded source-preview candidate.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileSelectionError::InvalidSourcePreview`] when the
    /// candidate has no source bytes or individually exceeds the shared
    /// source-preview ceiling.
    pub const fn source_preview(
        estimated_tokens: u64,
        source_bytes: u32,
    ) -> Result<Self, ProfileSelectionError> {
        if source_bytes == 0 || source_bytes > MAX_PROFILE_SOURCE_PREVIEW_BYTES {
            return Err(ProfileSelectionError::InvalidSourcePreview);
        }
        Ok(Self {
            kind: ProfileCandidateKind::SourcePreview,
            estimated_tokens,
            source_bytes: source_bytes as u64,
        })
    }

    /// Returns this candidate's representation class.
    #[must_use]
    pub const fn kind(self) -> ProfileCandidateKind {
        self.kind
    }

    /// Returns the deterministic output-token estimate charged on selection.
    #[must_use]
    pub const fn estimated_tokens(self) -> u64 {
        self.estimated_tokens
    }

    /// Returns raw source bytes charged on selection.
    #[must_use]
    pub const fn source_bytes(self) -> u64 {
        self.source_bytes
    }
}

/// Counts of optional representation candidates omitted by one shaping pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfileOmissions {
    profile_limit: u32,
    source_policy: u32,
    budget: u32,
}

impl ProfileOmissions {
    /// Returns candidates omitted by profile cardinality or byte ceilings.
    #[must_use]
    pub const fn profile_limit(self) -> u32 {
        self.profile_limit
    }

    /// Returns candidates omitted because the tool prohibits their source class.
    #[must_use]
    pub const fn source_policy(self) -> u32 {
        self.source_policy
    }

    /// Returns candidates omitted because the admitted shared budget was full.
    #[must_use]
    pub const fn budget(self) -> u32 {
        self.budget
    }

    /// Reports whether the requested representation was degraded.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.profile_limit == 0 && self.source_policy == 0 && self.budget == 0
    }
}

/// Stable selected candidate indices and their charged representation usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSelection {
    selected_indices: Vec<usize>,
    omissions: ProfileOmissions,
    usage_delta: BudgetCharge,
}

impl ProfileSelection {
    /// Returns selected candidate indices in original deterministic order.
    #[must_use]
    pub fn selected_indices(&self) -> &[usize] {
        &self.selected_indices
    }

    /// Returns source-free omission counts for warning/recovery construction.
    #[must_use]
    pub const fn omissions(&self) -> ProfileOmissions {
        self.omissions
    }

    /// Returns resource usage atomically committed to the shared ledger.
    #[must_use]
    pub const fn usage_delta(&self) -> BudgetCharge {
        self.usage_delta
    }

    /// Reports whether optional representation detail was omitted.
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        !self.omissions.is_empty()
    }
}

/// Failure while selecting optional representation detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProfileSelectionError {
    /// The candidate collection exceeded the bounded planner ceiling.
    #[error("response profile candidate collection is too large")]
    TooManyCandidates,
    /// One source preview was empty or exceeded the shared byte ceiling.
    #[error("response profile source preview is invalid")]
    InvalidSourcePreview,
    /// Cooperative cancellation stopped representation selection.
    #[error("response profile shaping was cancelled")]
    Cancelled,
    /// The bounded selected-index allocation could not be reserved.
    #[error("response profile shaping memory is unavailable")]
    MemoryUnavailable,
}

/// Selects optional typed representation detail under one shared execution context.
///
/// The selector preserves candidate order and can charge only output tokens and
/// source bytes. It cannot widen result, traversal, depth, path, or time
/// budgets, and a rejected charge leaves the shared ledger unchanged.
///
/// # Errors
///
/// Returns [`ProfileSelectionError::TooManyCandidates`] for an oversized
/// candidate slice, [`ProfileSelectionError::Cancelled`] after cooperative
/// cancellation, or [`ProfileSelectionError::MemoryUnavailable`] when the
/// bounded index allocation fails.
pub fn select_profile_candidates<C>(
    candidates: &[ProfileCandidate],
    policies: ProfileLimitSet,
    context: &mut ExecutionContext<C>,
) -> Result<ProfileSelection, ProfileSelectionError>
where
    C: CancellationSignal,
{
    if candidates.len() > MAX_PROFILE_CANDIDATES {
        return Err(ProfileSelectionError::TooManyCandidates);
    }
    let limits = policies.limits(context.profile());
    let mut selected_indices = Vec::new();
    selected_indices
        .try_reserve_exact(candidates.len())
        .map_err(|_| ProfileSelectionError::MemoryUnavailable)?;
    let mut omissions = ProfileOmissions::default();
    let mut usage_delta = BudgetCharge::default();
    let mut rationales = 0_u16;
    let mut evidence_references = 0_u16;
    let mut source_preview_bytes = 0_u64;

    for (index, candidate) in candidates.iter().copied().enumerate() {
        context
            .checkpoint()
            .map_err(|_| ProfileSelectionError::Cancelled)?;
        match candidate.kind {
            ProfileCandidateKind::Rationale if rationales >= limits.rationale_items_per_result => {
                omissions.profile_limit = omissions.profile_limit.saturating_add(1);
                continue;
            }
            ProfileCandidateKind::EvidenceReference
                if !matches!(
                    limits.source_access,
                    ProfileSourceAccess::References | ProfileSourceAccess::Snippets
                ) =>
            {
                omissions.source_policy = omissions.source_policy.saturating_add(1);
                continue;
            }
            ProfileCandidateKind::EvidenceReference
                if evidence_references >= limits.evidence_references_per_result =>
            {
                omissions.profile_limit = omissions.profile_limit.saturating_add(1);
                continue;
            }
            ProfileCandidateKind::SourcePreview
                if !matches!(limits.source_access, ProfileSourceAccess::Snippets) =>
            {
                omissions.source_policy = omissions.source_policy.saturating_add(1);
                continue;
            }
            ProfileCandidateKind::SourcePreview
                if source_preview_bytes.saturating_add(candidate.source_bytes)
                    > u64::from(limits.source_preview_bytes) =>
            {
                omissions.profile_limit = omissions.profile_limit.saturating_add(1);
                continue;
            }
            ProfileCandidateKind::Rationale
            | ProfileCandidateKind::EvidenceReference
            | ProfileCandidateKind::SourcePreview => {}
        }

        let charge = BudgetCharge {
            tokens: candidate.estimated_tokens,
            source_bytes: candidate.source_bytes,
            ..BudgetCharge::default()
        };
        match context.budget_mut().charge(charge) {
            Ok(()) => {}
            Err(ExecutionPolicyError::BudgetExceeded { .. }) => {
                omissions.budget = omissions.budget.saturating_add(1);
                continue;
            }
            Err(ExecutionPolicyError::Cancelled) => {
                return Err(ProfileSelectionError::Cancelled);
            }
        }
        selected_indices.push(index);
        usage_delta.tokens = usage_delta
            .tokens
            .saturating_add(candidate.estimated_tokens);
        usage_delta.source_bytes = usage_delta
            .source_bytes
            .saturating_add(candidate.source_bytes);
        match candidate.kind {
            ProfileCandidateKind::Rationale => rationales = rationales.saturating_add(1),
            ProfileCandidateKind::EvidenceReference => {
                evidence_references = evidence_references.saturating_add(1);
            }
            ProfileCandidateKind::SourcePreview => {
                source_preview_bytes = source_preview_bytes.saturating_add(candidate.source_bytes);
            }
        }
    }

    Ok(ProfileSelection {
        selected_indices,
        omissions,
        usage_delta,
    })
}

/// Typed analytical data that can be projected to a response profile.
///
/// Implementations must preserve semantic identities, ordering, aggregate
/// counts, confidence needed to interpret claims, trust, uncertainty, recovery
/// handles, gaps, blind spots, and explain-mode output. They may only remove
/// approved optional metadata or shorten bounded rationale/evidence prefixes.
pub trait ProfileShape {
    /// Applies one profile using a caller-provided validated monotone policy.
    fn shape_with_limits(&mut self, profile: ResponseProfile, limits: ProfileLimits);

    /// Applies one profile using the reviewed analytical defaults.
    fn shape(&mut self, profile: ResponseProfile) {
        self.shape_with_limits(profile, ProfileLimitSet::analytical().limits(profile));
    }
}

/// Captured transport and recovery facts that shaping must preserve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseInvariants<Identity, Recovery> {
    identity: Identity,
    completeness: ResultCompleteness,
    trust: TrustClassification,
    warnings: Vec<ResponseWarning>,
    recovery: Recovery,
}

impl<Identity, Recovery> ResponseInvariants<Identity, Recovery> {
    /// Captures identity, completeness, trust, warnings, and recovery facts.
    #[must_use]
    pub fn new(
        identity: Identity,
        completeness: ResultCompleteness,
        trust: TrustClassification,
        warnings: Vec<ResponseWarning>,
        recovery: Recovery,
    ) -> Self {
        Self {
            identity,
            completeness,
            trust,
            warnings,
            recovery,
        }
    }

    /// Validates that a shaped result retained every correctness-bearing fact.
    ///
    /// Additional warnings are allowed so budget degradation can remain
    /// explicit, but existing warnings cannot disappear.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileInvariantError`] for the first changed or removed
    /// invariant family.
    pub fn validate_preserved(&self, shaped: &Self) -> Result<(), ProfileInvariantError>
    where
        Identity: Eq,
        Recovery: Eq,
    {
        if self.identity != shaped.identity {
            return Err(ProfileInvariantError::IdentityChanged);
        }
        if self.completeness != shaped.completeness {
            return Err(ProfileInvariantError::CompletenessChanged);
        }
        if self.trust != shaped.trust {
            return Err(ProfileInvariantError::TrustChanged);
        }
        if self
            .warnings
            .iter()
            .any(|warning| !shaped.warnings.contains(warning))
        {
            return Err(ProfileInvariantError::WarningRemoved);
        }
        if self.recovery != shaped.recovery {
            return Err(ProfileInvariantError::RecoveryChanged);
        }
        Ok(())
    }
}

/// Correctness-bearing fact changed during representation shaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProfileInvariantError {
    /// Repository, generation, semantic entity, or ordering identity changed.
    #[error("response profile changed semantic identity")]
    IdentityChanged,
    /// Execution completeness or limiting-resource truth changed.
    #[error("response profile changed completeness")]
    CompletenessChanged,
    /// Repository-data trust classification changed.
    #[error("response profile changed trust")]
    TrustChanged,
    /// A pre-existing warning was removed.
    #[error("response profile removed a warning")]
    WarningRemoved,
    /// A continuation, gap, blind spot, or recovery handle changed.
    #[error("response profile changed recovery guidance")]
    RecoveryChanged,
}

/// Failure while projecting one canonical batch child into its public profile.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BatchProfileProjectionError {
    /// A fixed-profile child cannot satisfy the aggregate profile.
    #[error("batch child does not support the requested response profile")]
    UnsupportedProfile,
    /// Canonical child data did not satisfy its advertised typed contract.
    #[error("batch child returned invalid canonical data")]
    InvalidData(#[source] serde_json::Error),
}

/// Projects canonical typed batch-child data into its public representation.
///
/// Callers retain the original value for typed binding resolution and publish
/// only the returned copy. Fixed compact children are passed through after the
/// aggregate profile is validated.
///
/// # Errors
///
/// Returns [`BatchProfileProjectionError::UnsupportedProfile`] when a
/// fixed-profile child receives a non-compact aggregate profile, or
/// [`BatchProfileProjectionError::InvalidData`] when canonical data violates
/// the child's typed response contract.
pub fn shape_batch_child_data(
    tool: BatchTool,
    canonical: &Value,
    profile: ResponseProfile,
) -> Result<Value, BatchProfileProjectionError> {
    macro_rules! project {
        ($data_type:ty) => {{
            let mut typed: $data_type = serde_json::from_value(canonical.clone())
                .map_err(BatchProfileProjectionError::InvalidData)?;
            typed.shape(profile);
            serde_json::to_value(typed).map_err(BatchProfileProjectionError::InvalidData)
        }};
    }
    macro_rules! validate {
        ($data_type:ty) => {{
            let typed: $data_type = serde_json::from_value(canonical.clone())
                .map_err(BatchProfileProjectionError::InvalidData)?;
            serde_json::to_value(typed).map_err(BatchProfileProjectionError::InvalidData)
        }};
    }

    match tool {
        BatchTool::CodeLocate => project!(CodeLocateData),
        BatchTool::SymbolExplain => project!(SymbolExplainData),
        BatchTool::SymbolRelationships => project!(SymbolRelationshipsData),
        BatchTool::FlowTrace => project!(FlowTraceData),
        BatchTool::ChangeImpact => project!(ChangeImpactData),
        BatchTool::TestsSelect => project!(TestsSelectData),
        BatchTool::ArchitectureOverview => project!(ArchitectureOverviewData),
        BatchTool::ArchitectureCycles => project!(ArchitectureCyclesData),
        BatchTool::CodeDead => project!(CodeDeadData),
        BatchTool::PlanChange => project!(PlanChangeData),
        BatchTool::ContextPack => project!(ContextPackData),
        BatchTool::SourceRead => {
            if supports_profile(McpTool::SourceRead, profile) {
                validate!(SourceReadData)
            } else {
                Err(BatchProfileProjectionError::UnsupportedProfile)
            }
        }
    }
}

fn supports_profile(tool: McpTool, profile: ResponseProfile) -> bool {
    match capability_for(tool).response_profiles {
        ResponseProfileSupport::Fixed { representation } => representation == profile,
        ResponseProfileSupport::Selectable { supported, .. } => supported.contains(&profile),
    }
}

/// Shapes only tool data inside a read envelope.
///
/// Repository/generation identity, completeness, continuation, usage, warnings,
/// and envelope trust are structurally unreachable to the shaper.
pub fn shape_read_envelope<T>(envelope: &mut ReadEnvelope<T>, profile: ResponseProfile)
where
    T: ProfileShape,
{
    envelope.data.shape(profile);
}

/// Shapes one typed analytical result without transport-specific branching.
pub fn shape_data<T>(data: &mut T, profile: ResponseProfile)
where
    T: ProfileShape,
{
    data.shape(profile);
}

impl ProfileShape for CodeLocateData {
    fn shape_with_limits(&mut self, profile: ResponseProfile, limits: ProfileLimits) {
        let rationale_limit = usize::from(limits.rationale_items_per_result);
        for item in &mut self.matches {
            item.why.truncate(rationale_limit);
            if !limits.optional_fields.signatures {
                item.signature = None;
            }
            if limits.evidence_references_per_result == 0 {
                item.source_ref = None;
            } else if let Some(source_ref) = &mut item.source_ref {
                shape_source_ref(source_ref, profile);
            }
        }
    }
}

impl ProfileShape for ContextPackData {
    fn shape_with_limits(&mut self, profile: ResponseProfile, _limits: ProfileLimits) {
        for item in &mut self.items {
            match profile {
                ResponseProfile::Compact => item.snippet = None,
                ResponseProfile::Standard => {
                    if let Some(snippet) = &mut item.snippet
                        && truncate_utf8(&mut snippet.content, STANDARD_CONTEXT_SNIPPET_BYTES)
                    {
                        snippet.truncated = true;
                    }
                }
                ResponseProfile::Evidence => {}
            }
        }
    }
}

impl ProfileShape for SymbolExplainData {
    fn shape_with_limits(&mut self, profile: ResponseProfile, limits: ProfileLimits) {
        let evidence_limit = usize::from(limits.evidence_references_per_result);
        for symbol in &mut self.symbols {
            if !limits.optional_fields.signatures {
                symbol.signature = None;
            }
            shape_source_ref(&mut symbol.definition, profile);
            symbol.provenance.truncate(evidence_limit);
        }
    }
}

impl ProfileShape for SymbolRelationshipsData {
    fn shape_with_limits(&mut self, profile: ResponseProfile, limits: ProfileLimits) {
        let evidence_limit = usize::from(limits.evidence_references_per_result);
        for group in &mut self.groups {
            for item in &mut group.items {
                shape_source_refs(&mut item.source_refs, profile, evidence_limit);
                let remaining = evidence_limit.saturating_sub(item.source_refs.len());
                item.provenance.truncate(remaining);
            }
        }
    }
}

impl ProfileShape for FlowTraceData {
    fn shape_with_limits(&mut self, profile: ResponseProfile, limits: ProfileLimits) {
        let evidence_limit = usize::from(limits.evidence_references_per_result);
        for path in &mut self.paths {
            for edge in &mut path.edges {
                shape_source_refs(&mut edge.source_refs, profile, evidence_limit);
            }
        }
    }
}

impl ProfileShape for ChangeImpactData {
    fn shape_with_limits(&mut self, _profile: ResponseProfile, limits: ProfileLimits) {
        let rationale_limit = usize::from(limits.rationale_items_per_result);
        if !limits.optional_fields.optional_metadata {
            for change in &mut self.resolved_changes {
                change.kind = None;
            }
        }
        for test in &mut self.tests {
            shape_test_candidate(test, rationale_limit, limits.optional_fields);
        }
    }
}

impl ProfileShape for TestsSelectData {
    fn shape_with_limits(&mut self, _profile: ResponseProfile, limits: ProfileLimits) {
        let rationale_limit = usize::from(limits.rationale_items_per_result);
        for test in &mut self.tests {
            test.why.truncate(rationale_limit);
            if !limits.optional_fields.cost_hints {
                test.estimated_cost_ms = None;
            }
            if !limits.optional_fields.optional_metadata {
                test.path = None;
                test.command_hint = None;
            }
        }
    }
}

impl ProfileShape for ArchitectureOverviewData {
    fn shape_with_limits(&mut self, _profile: ResponseProfile, limits: ProfileLimits) {
        let rationale_limit = usize::from(limits.rationale_items_per_result);
        for component in &mut self.components {
            component.responsibility_evidence.truncate(rationale_limit);
        }
        if !limits.optional_fields.optional_metadata {
            for hotspot in &mut self.hotspots {
                hotspot.change_frequency = None;
                hotspot.complexity = None;
            }
        }
    }
}

impl ProfileShape for ArchitectureCyclesData {
    fn shape_with_limits(&mut self, profile: ResponseProfile, limits: ProfileLimits) {
        let evidence_limit = usize::from(limits.evidence_references_per_result);
        for cycle in &mut self.cycles {
            shape_source_refs(&mut cycle.edge_evidence, profile, evidence_limit);
        }
        for candidate in &mut self.break_candidates {
            shape_source_refs(&mut candidate.source_refs, profile, evidence_limit);
        }
    }
}

impl ProfileShape for CodeDeadData {
    fn shape_with_limits(&mut self, profile: ResponseProfile, limits: ProfileLimits) {
        let rationale_limit = usize::from(limits.rationale_items_per_result);
        let evidence_limit = usize::from(limits.evidence_references_per_result);
        for candidate in &mut self.candidates {
            candidate.why.truncate(rationale_limit);
            shape_source_refs(&mut candidate.source_refs, profile, evidence_limit);
        }
    }
}

impl ProfileShape for PlanChangeData {
    fn shape_with_limits(&mut self, _profile: ResponseProfile, limits: ProfileLimits) {
        let rationale_limit = usize::from(limits.rationale_items_per_result);
        for step in &mut self.plan {
            if !limits.optional_fields.verification_hints {
                step.verification = None;
            }
        }
        for test in &mut self.test_plan {
            shape_test_candidate(test, rationale_limit, limits.optional_fields);
        }
    }
}

fn shape_test_candidate(
    candidate: &mut TestCandidate,
    rationale_limit: usize,
    optional_fields: OptionalProfileFields,
) {
    candidate.why.truncate(rationale_limit);
    if !optional_fields.cost_hints {
        candidate.estimated_cost_ms = None;
    }
}

fn shape_source_refs(source_refs: &mut Vec<SourceRef>, profile: ResponseProfile, limit: usize) {
    source_refs.truncate(limit);
    for source_ref in source_refs {
        shape_source_ref(source_ref, profile);
    }
}

fn shape_source_ref(source_ref: &mut SourceRef, profile: ResponseProfile) {
    if !matches!(profile, ResponseProfile::Evidence) && source_ref.line_hint().is_some() {
        *source_ref = SourceRef::new(
            source_ref.repository(),
            source_ref.generation(),
            source_ref.span(),
            source_ref.content_hash(),
            None,
        );
    }
}

fn truncate_utf8(value: &mut String, maximum_bytes: usize) -> bool {
    if value.len() <= maximum_bytes {
        return false;
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    true
}

const fn source_access_rank(access: ProfileSourceAccess) -> u8 {
    match access {
        ProfileSourceAccess::None => 0,
        ProfileSourceAccess::References => 1,
        ProfileSourceAccess::Snippets => 2,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::policy::{BudgetLedger, NeverCancelled};
    use proptest::prelude::*;
    use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId, SymbolId};
    use rootlight_ir::{CoverageStatus, EntityKind, LineRange, SourceSpan};
    use rootlight_mcp_contract::{
        SafeLabel,
        change::{
            ChangePlanStep, ContextPackRequest, ImpactEntry, ImpactGroup, ImpactRiskSummary,
            PlanDecision, PlanImpactSummary, RankedTest, RiskLevel, TestCandidate,
            TestCoverageStrategy, TestGap, TestKind,
        },
        intent::{
            ArchitectureComponent, ArchitectureConnection, ArchitectureView, BlindSpot,
            CycleBreakCandidate, DeadCandidate, DeadClassification, DerivedViewInfo, Direction,
            EntryPointPolicy, EntryPointSummary, FlowTraceData, FrontierSummary, Hotspot,
            MinimalCycle, RelationKind, RelationProjection, RelationshipGroup, RelationshipTarget,
            RelationshipTotals, RuleSummary, StronglyConnectedComponent, TraceEdge, TracePath,
            UnresolvedSiteSummary,
        },
        vertical::{
            CodeLocateData, DetailHandle, EntityKind as ContractEntityKind, LocateReason,
            LocatedItem, ProvenanceSummary, QueryInterpretation, RelationSummary, ResponseBudget,
            SearchMode, SourceFreeMessage, SuggestedTool, SymbolExplainData, SymbolExplanation,
            ToolSuggestion,
        },
    };

    fn limits(
        rationale_items: u16,
        evidence_references: u16,
        source_preview_bytes: u32,
        source_access: ProfileSourceAccess,
        optional_fields: OptionalProfileFields,
    ) -> ProfileLimits {
        ProfileLimits::new(
            rationale_items,
            evidence_references,
            source_preview_bytes,
            source_access,
            optional_fields,
        )
        .expect("test profile limits should be valid")
    }

    fn test_selection_data(
        rationale_count: usize,
        include_optional_fields: bool,
    ) -> TestsSelectData {
        TestsSelectData {
            tests: vec![RankedTest {
                test_id: "unit-agent-profile".to_owned(),
                kind: TestKind::Unit,
                path: include_optional_fields.then(|| "tests/agent_profile.rs".to_owned()),
                score: 975,
                why: (0..rationale_count)
                    .map(|index| format!("reason-{index}"))
                    .collect(),
                estimated_cost_ms: include_optional_fields.then_some(25),
                command_hint: include_optional_fields
                    .then(|| "cargo test -p rootlight-agent".to_owned()),
            }],
            coverage_strategy: TestCoverageStrategy {
                direct_edges: true,
                transitive_signals: true,
                history_signals: false,
                build_target_signals: true,
            },
            gaps: vec![TestGap {
                scope: "platform-specific-cancellation".to_owned(),
                reason: SafeLabel::parse("coverage-gap").expect("safe test label"),
            }],
            explanation: None,
        }
    }

    fn warning(code: &str) -> ResponseWarning {
        ResponseWarning {
            code: SafeLabel::parse(code).expect("safe test warning code"),
            message: SourceFreeMessage::parse("optional response detail omitted")
                .expect("safe test warning message"),
        }
    }

    fn source_ref(seed: u8) -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([seed; 16]),
            GenerationId::from_bytes([seed.saturating_add(1); 20]),
            SourceSpan::new(
                FileId::from_bytes([seed.saturating_add(2); 20]),
                u64::from(seed),
                u64::from(seed).saturating_add(8),
            )
            .expect("valid test source span"),
            ContentHash::from_bytes([seed.saturating_add(3); 32]),
            Some(
                LineRange::new(
                    u64::from(seed).saturating_add(1),
                    u64::from(seed).saturating_add(2),
                )
                .expect("valid test line range"),
            ),
        )
    }

    fn source_refs(count: u8) -> Vec<SourceRef> {
        (1..=count).map(source_ref).collect()
    }

    fn assert_source_semantics(actual: &SourceRef, expected: &SourceRef) {
        assert_eq!(actual.repository(), expected.repository());
        assert_eq!(actual.generation(), expected.generation());
        assert_eq!(actual.span(), expected.span());
        assert_eq!(actual.content_hash(), expected.content_hash());
    }

    fn shaped_profiles<T>(raw: &T) -> (T, T, T)
    where
        T: ProfileShape + Clone,
    {
        let mut compact = raw.clone();
        let mut standard = raw.clone();
        let mut evidence = raw.clone();
        compact.shape(ResponseProfile::Compact);
        standard.shape(ResponseProfile::Standard);
        evidence.shape(ResponseProfile::Evidence);
        (compact, standard, evidence)
    }

    #[test]
    fn supported_analytical_types_implement_profile_shape() {
        fn assert_profile_shape<T: ProfileShape>() {}

        assert_profile_shape::<CodeLocateData>();
        assert_profile_shape::<SymbolExplainData>();
        assert_profile_shape::<SymbolRelationshipsData>();
        assert_profile_shape::<FlowTraceData>();
        assert_profile_shape::<ChangeImpactData>();
        assert_profile_shape::<TestsSelectData>();
        assert_profile_shape::<ArchitectureOverviewData>();
        assert_profile_shape::<ArchitectureCyclesData>();
        assert_profile_shape::<CodeDeadData>();
        assert_profile_shape::<PlanChangeData>();
        assert_profile_shape::<ContextPackData>();
    }

    #[test]
    fn batch_projection_shapes_a_copy_and_rejects_unsupported_profile() {
        let canonical = serde_json::to_value(test_selection_data(4, true))
            .expect("canonical test data should serialize");

        let projected =
            shape_batch_child_data(BatchTool::TestsSelect, &canonical, ResponseProfile::Compact)
                .expect("compact test selection should project");
        let projected: TestsSelectData =
            serde_json::from_value(projected).expect("projected data should remain typed");

        assert_eq!(projected.tests[0].why, ["reason-0"]);
        assert_eq!(projected.tests[0].path, None);
        assert_eq!(projected.tests[0].estimated_cost_ms, None);
        assert_eq!(projected.tests[0].command_hint, None);
        assert!(
            canonical["tests"][0]["path"].is_string(),
            "canonical binding data must remain unchanged"
        );

        assert!(matches!(
            shape_batch_child_data(
                BatchTool::SourceRead,
                &serde_json::json!({}),
                ResponseProfile::Standard,
            ),
            Err(BatchProfileProjectionError::UnsupportedProfile)
        ));
    }

    #[test]
    fn profile_limit_set_rejects_non_monotone_and_non_evidence_source_bodies() {
        let no_optional = OptionalProfileFields::default();
        let all_optional = OptionalProfileFields::new(true, true, true, true);
        let compact = limits(2, 1, 0, ProfileSourceAccess::References, no_optional);
        let narrower_standard = limits(1, 1, 0, ProfileSourceAccess::References, all_optional);
        let evidence = limits(4, 4, 0, ProfileSourceAccess::References, all_optional);

        assert_eq!(
            ProfileLimitSet::new(compact, narrower_standard, evidence),
            Err(ProfilePolicyError::NonMonotoneCompactToStandard)
        );

        let compact_with_source_body = limits(2, 1, 32, ProfileSourceAccess::Snippets, no_optional);
        assert_eq!(
            ProfileLimitSet::new(compact_with_source_body, evidence, evidence),
            Err(ProfilePolicyError::SourcePreviewOutsideEvidence)
        );
    }

    #[test]
    fn profile_selection_is_stable_and_respects_source_policy() {
        let no_optional = OptionalProfileFields::default();
        let compact = limits(1, 0, 0, ProfileSourceAccess::None, no_optional);
        let standard = limits(2, 2, 0, ProfileSourceAccess::References, no_optional);
        let evidence = limits(2, 2, 64, ProfileSourceAccess::Snippets, no_optional);
        let policies =
            ProfileLimitSet::new(compact, standard, evidence).expect("monotone test policy");
        let candidates = [
            ProfileCandidate::rationale(1),
            ProfileCandidate::evidence_reference(1),
            ProfileCandidate::source_preview(1, 16).expect("bounded source preview"),
            ProfileCandidate::rationale(1),
        ];

        let mut compact_context = ExecutionContext::new(
            ResponseProfile::Compact,
            BudgetLedger::new(None),
            NeverCancelled,
        );
        let compact_selection =
            select_profile_candidates(&candidates, policies, &mut compact_context)
                .expect("compact selection");
        assert_eq!(compact_selection.selected_indices(), &[0]);
        assert_eq!(compact_selection.omissions().source_policy(), 2);
        assert_eq!(compact_selection.omissions().profile_limit(), 1);

        let mut standard_context = ExecutionContext::new(
            ResponseProfile::Standard,
            BudgetLedger::new(None),
            NeverCancelled,
        );
        let standard_selection =
            select_profile_candidates(&candidates, policies, &mut standard_context)
                .expect("standard selection");
        assert_eq!(standard_selection.selected_indices(), &[0, 1, 3]);
        assert_eq!(standard_selection.omissions().source_policy(), 1);

        let mut evidence_context = ExecutionContext::new(
            ResponseProfile::Evidence,
            BudgetLedger::new(None),
            NeverCancelled,
        );
        let evidence_selection =
            select_profile_candidates(&candidates, policies, &mut evidence_context)
                .expect("evidence selection");
        assert_eq!(evidence_selection.selected_indices(), &[0, 1, 2, 3]);
        assert!(evidence_selection.omissions().is_empty());
    }

    #[test]
    fn evidence_selection_cannot_bypass_shared_hard_budget() {
        let budget = ResponseBudget {
            max_results: Some(1),
            max_tokens: Some(100),
            max_source_bytes: Some(20),
            max_traversal_facts: Some(10),
            max_depth: Some(2),
            max_paths: Some(1),
            timeout_ms: Some(100),
            evidence_level: None,
        };
        let mut ledger = BudgetLedger::new(Some(budget));
        ledger
            .charge(BudgetCharge {
                results: 1,
                traversal_facts: 9,
                depth: 2,
                paths: 1,
                time_ms: 80,
                ..BudgetCharge::default()
            })
            .expect("semantic work should fit its admitted budget");
        let before = ledger.consumed();
        let mut context = ExecutionContext::new(ResponseProfile::Evidence, ledger, NeverCancelled);
        let candidates = [
            ProfileCandidate::rationale(60),
            ProfileCandidate::source_preview(30, 16).expect("bounded source preview"),
            ProfileCandidate::evidence_reference(50),
        ];
        let no_optional = OptionalProfileFields::default();
        let compact = limits(1, 0, 0, ProfileSourceAccess::None, no_optional);
        let standard = limits(2, 2, 0, ProfileSourceAccess::References, no_optional);
        let evidence = limits(3, 3, 20, ProfileSourceAccess::Snippets, no_optional);
        let policies =
            ProfileLimitSet::new(compact, standard, evidence).expect("monotone test policy");

        let selection = select_profile_candidates(&candidates, policies, &mut context)
            .expect("budget exhaustion degrades optional detail");
        let after = context.budget().consumed();

        assert_eq!(selection.selected_indices(), &[0, 1]);
        assert_eq!(selection.omissions().budget(), 1);
        assert_eq!(after.results, before.results);
        assert_eq!(after.traversal_facts, before.traversal_facts);
        assert_eq!(after.depth, before.depth);
        assert_eq!(after.paths, before.paths);
        assert_eq!(after.time_ms, before.time_ms);
        assert_eq!(after.tokens, 90);
        assert_eq!(after.source_bytes, 16);
    }

    #[test]
    fn tests_select_profiles_are_monotone_and_preserve_semantic_fields() {
        let raw = test_selection_data(8, true);
        let recovery = (raw.coverage_strategy.clone(), raw.gaps.clone());
        let mut compact = raw.clone();
        let mut standard = raw.clone();
        let mut evidence = raw;

        shape_data(&mut compact, ResponseProfile::Compact);
        shape_data(&mut standard, ResponseProfile::Standard);
        shape_data(&mut evidence, ResponseProfile::Evidence);

        assert_eq!(compact.tests[0].why.len(), 1);
        assert_eq!(standard.tests[0].why.len(), 4);
        assert_eq!(evidence.tests[0].why.len(), 8);
        assert!(compact.tests[0].path.is_none());
        assert!(compact.tests[0].estimated_cost_ms.is_none());
        assert!(compact.tests[0].command_hint.is_none());
        assert!(standard.tests[0].path.is_some());
        assert!(evidence.tests[0].command_hint.is_some());
        assert_eq!(
            (compact.coverage_strategy.clone(), compact.gaps.clone()),
            recovery
        );
        assert_eq!(
            (standard.coverage_strategy.clone(), standard.gaps.clone()),
            recovery
        );
        assert_eq!(
            (evidence.coverage_strategy.clone(), evidence.gaps.clone()),
            recovery
        );
        assert_eq!(compact.tests[0].test_id, standard.tests[0].test_id);
        assert_eq!(standard.tests[0].test_id, evidence.tests[0].test_id);
        assert_eq!(compact.tests[0].kind, standard.tests[0].kind);
        assert_eq!(standard.tests[0].kind, evidence.tests[0].kind);
        assert_eq!(compact.tests[0].score, standard.tests[0].score);
        assert_eq!(standard.tests[0].score, evidence.tests[0].score);
    }

    #[test]
    fn compact_code_dead_shape_preserves_recovery_and_false_positive_controls() {
        let entry_points = EntryPointSummary {
            policy: EntryPointPolicy::Application,
            entry_point_count: 7,
            complete: false,
        };
        let blind_spots = vec![BlindSpot {
            category: "dynamic-dispatch".to_owned(),
            affected_count: 3,
        }];
        let false_positive_controls = vec![RuleSummary {
            rule: "exported-symbol".to_owned(),
            suppressed_count: 5,
        }];
        let suppressions_checked = vec![
            "entry-point".to_owned(),
            "public-export".to_owned(),
            "reflection-hook".to_owned(),
        ];
        let mut data = CodeDeadData {
            candidates: vec![DeadCandidate {
                symbol_id: SymbolId::from_bytes([9; 20]),
                classification: DeadClassification::ProbableDead,
                confidence: 900,
                why: vec!["unreachable".to_owned(), "unreferenced".to_owned()],
                suppressions_checked: suppressions_checked.clone(),
                source_refs: Vec::new(),
                trust: TrustClassification::UntrustedRepositoryData,
            }],
            entry_points: entry_points.clone(),
            blind_spots: blind_spots.clone(),
            false_positive_controls: false_positive_controls.clone(),
            explanation: None,
        };

        data.shape(ResponseProfile::Compact);

        assert_eq!(data.entry_points, entry_points);
        assert_eq!(data.blind_spots, blind_spots);
        assert_eq!(data.false_positive_controls, false_positive_controls);
        assert_eq!(data.candidates[0].why, ["unreachable"]);
        assert_eq!(
            data.candidates[0].suppressions_checked,
            suppressions_checked
        );
    }

    #[test]
    fn compact_change_impact_preserves_the_complete_relation_path() {
        let via = vec![
            "calls".to_owned(),
            "implements".to_owned(),
            "depends_on".to_owned(),
        ];
        let mut data = ChangeImpactData {
            resolved_changes: Vec::new(),
            impacted: vec![ImpactGroup {
                source_index: 0,
                dependents: vec![ImpactEntry {
                    symbol_id: SymbolId::from_bytes([8; 20]),
                    kind: EntityKind::Function,
                    distance: 3,
                    confidence: 875,
                    via: via.clone(),
                    is_public: true,
                }],
            }],
            service_impacts: Vec::new(),
            tests: Vec::new(),
            risk_summary: ImpactRiskSummary {
                level: RiskLevel::Medium,
                reasons: vec!["public-surface".to_owned(), "transitive".to_owned()],
                coverage: CoverageStatus::Complete,
                breaking_surface: true,
                fanout: 1,
                dynamic_blind_spots: false,
            },
            explanation: None,
        };

        data.shape(ResponseProfile::Compact);

        assert_eq!(data.impacted[0].dependents[0].distance, 3);
        assert_eq!(data.impacted[0].dependents[0].via, via);
        assert_eq!(data.risk_summary.reasons, ["public-surface", "transitive"]);
    }

    #[test]
    fn code_locate_profiles_expand_detail_without_changing_ranked_semantics() {
        let symbol_id = SymbolId::from_bytes([10; 20]);
        let file_id = FileId::from_bytes([11; 20]);
        let evidence = source_ref(12);
        let raw = CodeLocateData {
            matches: vec![LocatedItem {
                symbol_id: Some(symbol_id),
                file_id: Some(file_id),
                kind: ContractEntityKind::Function,
                display_name: "profile_target".to_owned(),
                signature: Some("fn profile_target()".to_owned()),
                path: "src/profile.rs".to_owned(),
                score: 990,
                why: vec![
                    LocateReason::Identifier,
                    LocateReason::Lexical,
                    LocateReason::Docs,
                    LocateReason::Path,
                    LocateReason::Structural,
                ],
                source_ref: Some(evidence.clone()),
                trust: TrustClassification::UntrustedRepositoryData,
            }],
            query_interpretation: QueryInterpretation {
                tokens: vec!["profile".to_owned(), "target".to_owned()],
                modes: BTreeSet::from([SearchMode::Exact, SearchMode::Lexical]),
                semantic_available: true,
            },
            suggested_next: vec![ToolSuggestion {
                tool: SuggestedTool::SymbolExplain,
                symbol_ids: BTreeSet::from([symbol_id]),
                source_refs: vec![evidence.clone()],
            }],
            explanation: None,
        };
        let (compact, standard, profiled_evidence) = shaped_profiles(&raw);

        assert_eq!(compact.matches[0].why.len(), 1);
        assert_eq!(standard.matches[0].why.len(), 4);
        assert_eq!(profiled_evidence.matches[0].why.len(), 5);
        assert!(compact.matches[0].signature.is_none());
        assert!(standard.matches[0].signature.is_some());
        assert!(
            compact.matches[0]
                .source_ref
                .as_ref()
                .expect("compact source reference")
                .line_hint()
                .is_none()
        );
        assert!(
            profiled_evidence.matches[0]
                .source_ref
                .as_ref()
                .expect("evidence source reference")
                .line_hint()
                .is_some()
        );

        for shaped in [&compact, &standard, &profiled_evidence] {
            assert_eq!(shaped.matches.len(), raw.matches.len());
            assert_eq!(shaped.matches[0].symbol_id, Some(symbol_id));
            assert_eq!(shaped.matches[0].file_id, Some(file_id));
            assert_eq!(shaped.matches[0].kind, ContractEntityKind::Function);
            assert_eq!(shaped.matches[0].display_name, "profile_target");
            assert_eq!(shaped.matches[0].path, "src/profile.rs");
            assert_eq!(shaped.matches[0].score, 990);
            assert_eq!(
                shaped.matches[0].trust,
                TrustClassification::UntrustedRepositoryData
            );
            assert_source_semantics(
                shaped.matches[0]
                    .source_ref
                    .as_ref()
                    .expect("shaped source reference"),
                &evidence,
            );
            assert_eq!(shaped.query_interpretation, raw.query_interpretation);
            assert_eq!(shaped.suggested_next, raw.suggested_next);
        }
    }

    #[test]
    fn symbol_explain_profiles_expand_detail_without_changing_symbol_semantics() {
        let symbol_id = SymbolId::from_bytes([20; 20]);
        let definition = source_ref(21);
        let provenance: Vec<_> = (0_u16..6)
            .map(|index| ProvenanceSummary {
                provider: format!("provider-{index}"),
                evidence: format!("evidence-{index}"),
                confidence: 900 - index,
            })
            .collect();
        let raw = SymbolExplainData {
            symbols: vec![SymbolExplanation {
                symbol_id,
                kind: ContractEntityKind::Function,
                display_name: "explained_symbol".to_owned(),
                signature: Some("fn explained_symbol() -> bool".to_owned()),
                definition: definition.clone(),
                relations: RelationSummary {
                    outbound_exact: 2,
                    outbound_candidates: 3,
                    inbound_exact: 5,
                    inbound_candidates: 7,
                    references_exact: 11,
                },
                provenance,
                confidence: 925,
                uncertainty: vec![warning("dynamic-call-uncertain")],
                trust: TrustClassification::UntrustedRepositoryData,
            }],
            unresolved_ids: vec![SymbolId::from_bytes([22; 20])],
            detail_handles: vec![DetailHandle {
                handle: "detail-handle".to_owned(),
                kind: "source-preview".to_owned(),
            }],
            explanation: None,
        };
        let (compact, standard, evidence) = shaped_profiles(&raw);

        assert_eq!(compact.symbols[0].provenance.len(), 1);
        assert_eq!(standard.symbols[0].provenance.len(), 4);
        assert_eq!(evidence.symbols[0].provenance.len(), 6);
        assert!(compact.symbols[0].signature.is_none());
        assert!(standard.symbols[0].signature.is_some());
        assert!(compact.symbols[0].definition.line_hint().is_none());
        assert!(standard.symbols[0].definition.line_hint().is_none());
        assert!(evidence.symbols[0].definition.line_hint().is_some());

        for shaped in [&compact, &standard, &evidence] {
            assert_eq!(shaped.symbols.len(), raw.symbols.len());
            assert_eq!(shaped.symbols[0].symbol_id, symbol_id);
            assert_eq!(shaped.symbols[0].kind, ContractEntityKind::Function);
            assert_eq!(shaped.symbols[0].display_name, "explained_symbol");
            assert_eq!(shaped.symbols[0].relations, raw.symbols[0].relations);
            assert_eq!(shaped.symbols[0].confidence, 925);
            assert_eq!(shaped.symbols[0].uncertainty, raw.symbols[0].uncertainty);
            assert_eq!(
                shaped.symbols[0].trust,
                TrustClassification::UntrustedRepositoryData
            );
            assert_source_semantics(&shaped.symbols[0].definition, &definition);
            assert_eq!(shaped.unresolved_ids, raw.unresolved_ids);
            assert_eq!(shaped.detail_handles, raw.detail_handles);
        }
    }

    #[test]
    fn relationship_profiles_expand_evidence_without_changing_edge_semantics() {
        let seed = SymbolId::from_bytes([30; 20]);
        let target = SymbolId::from_bytes([31; 20]);
        let provenance: Vec<_> = (0_u16..3)
            .map(|index| ProvenanceSummary {
                provider: format!("provider-{index}"),
                evidence: format!("edge-{index}"),
                confidence: 850 - index,
            })
            .collect();
        let raw = SymbolRelationshipsData {
            groups: vec![RelationshipGroup {
                seed,
                relation: RelationKind::Calls,
                direction: Direction::Outbound,
                items: vec![RelationshipTarget {
                    symbol_id: target,
                    confidence: 880,
                    source_refs: source_refs(3),
                    provenance,
                    trust: TrustClassification::UntrustedRepositoryData,
                }],
                total_count: 9,
            }],
            unresolved: vec![UnresolvedSiteSummary {
                seed,
                relation: RelationKind::Calls,
                candidate_count: 2,
                reason: SourceFreeMessage::parse("dynamic-dispatch")
                    .expect("safe unresolved reason"),
            }],
            totals: RelationshipTotals {
                returned_edges: 1,
                total_edges: 9,
                exact: false,
            },
            explanation: None,
        };
        let (compact, standard, evidence) = shaped_profiles(&raw);
        let representation_len = |data: &SymbolRelationshipsData| {
            data.groups[0].items[0].source_refs.len() + data.groups[0].items[0].provenance.len()
        };

        assert_eq!(representation_len(&compact), 1);
        assert_eq!(representation_len(&standard), 4);
        assert_eq!(representation_len(&evidence), 6);
        assert!(
            compact.groups[0].items[0].source_refs[0]
                .line_hint()
                .is_none()
        );
        assert!(
            evidence.groups[0].items[0].source_refs[0]
                .line_hint()
                .is_some()
        );

        for shaped in [&compact, &standard, &evidence] {
            assert_eq!(shaped.groups.len(), raw.groups.len());
            assert_eq!(shaped.groups[0].seed, seed);
            assert_eq!(shaped.groups[0].relation, RelationKind::Calls);
            assert_eq!(shaped.groups[0].direction, Direction::Outbound);
            assert_eq!(shaped.groups[0].items.len(), 1);
            assert_eq!(shaped.groups[0].items[0].symbol_id, target);
            assert_eq!(shaped.groups[0].items[0].confidence, 880);
            assert_eq!(
                shaped.groups[0].items[0].trust,
                TrustClassification::UntrustedRepositoryData
            );
            assert_eq!(shaped.groups[0].total_count, 9);
            assert_eq!(shaped.unresolved, raw.unresolved);
            assert_eq!(shaped.totals, raw.totals);
            for (actual, expected) in shaped.groups[0].items[0]
                .source_refs
                .iter()
                .zip(&raw.groups[0].items[0].source_refs)
            {
                assert_source_semantics(actual, expected);
            }
        }
    }

    #[test]
    fn flow_profiles_expand_evidence_without_changing_path_semantics() {
        let nodes = vec![
            SymbolId::from_bytes([40; 20]),
            SymbolId::from_bytes([41; 20]),
            SymbolId::from_bytes([42; 20]),
        ];
        let raw = FlowTraceData {
            paths: vec![TracePath {
                confidence: 870,
                nodes: nodes.clone(),
                edges: vec![TraceEdge {
                    kind: RelationKind::DataFlow,
                    confidence: 860,
                    source_refs: source_refs(6),
                    trust: TrustClassification::UntrustedRepositoryData,
                }],
                cyclic: false,
            }],
            frontier: FrontierSummary {
                reached_nodes: 13,
                examined_edges: 21,
                truncated: true,
                unresolved_boundaries: 3,
            },
            projection: RelationProjection {
                relations: BTreeSet::from([RelationKind::DataFlow, RelationKind::Calls]),
                min_confidence: 700,
            },
            explanation: None,
        };
        let (compact, standard, evidence) = shaped_profiles(&raw);

        assert_eq!(compact.paths[0].edges[0].source_refs.len(), 1);
        assert_eq!(standard.paths[0].edges[0].source_refs.len(), 4);
        assert_eq!(evidence.paths[0].edges[0].source_refs.len(), 6);
        assert!(
            compact.paths[0].edges[0].source_refs[0]
                .line_hint()
                .is_none()
        );
        assert!(
            evidence.paths[0].edges[0].source_refs[0]
                .line_hint()
                .is_some()
        );

        for shaped in [&compact, &standard, &evidence] {
            assert_eq!(shaped.paths.len(), 1);
            assert_eq!(shaped.paths[0].nodes, nodes);
            assert_eq!(shaped.paths[0].edges.len(), 1);
            assert_eq!(shaped.paths[0].edges[0].kind, RelationKind::DataFlow);
            assert_eq!(shaped.paths[0].edges[0].confidence, 860);
            assert_eq!(
                shaped.paths[0].edges[0].trust,
                TrustClassification::UntrustedRepositoryData
            );
            assert_eq!(shaped.paths[0].confidence, 870);
            assert!(!shaped.paths[0].cyclic);
            assert_eq!(shaped.frontier, raw.frontier);
            assert_eq!(shaped.projection, raw.projection);
            for (actual, expected) in shaped.paths[0].edges[0]
                .source_refs
                .iter()
                .zip(&raw.paths[0].edges[0].source_refs)
            {
                assert_source_semantics(actual, expected);
            }
        }
    }

    #[test]
    fn overview_profiles_expand_detail_without_changing_architecture_semantics() {
        let raw = ArchitectureOverviewData {
            components: vec![ArchitectureComponent {
                id: "component-a".to_owned(),
                kind: "crate".to_owned(),
                name: "agent".to_owned(),
                symbol_count: 17,
                responsibility_evidence: (0..6)
                    .map(|index| format!("responsibility-{index}"))
                    .collect(),
                confidence: 940,
                trust: TrustClassification::UntrustedRepositoryData,
            }],
            connections: vec![ArchitectureConnection {
                from: "component-a".to_owned(),
                to: "component-b".to_owned(),
                kind: RelationKind::BuildDependency,
                weight: 5,
                confidence: 910,
            }],
            hotspots: vec![Hotspot {
                component_id: "component-a".to_owned(),
                fan_in: 8,
                fan_out: 13,
                change_frequency: Some(21),
                complexity: Some(34),
                score: 915,
            }],
            views: vec![DerivedViewInfo {
                view: ArchitectureView::Modules,
                algorithm_version: "modules-v1".to_owned(),
            }],
            explanation: None,
        };
        let (compact, standard, evidence) = shaped_profiles(&raw);

        assert_eq!(compact.components[0].responsibility_evidence.len(), 1);
        assert_eq!(standard.components[0].responsibility_evidence.len(), 4);
        assert_eq!(evidence.components[0].responsibility_evidence.len(), 6);
        assert!(compact.hotspots[0].change_frequency.is_none());
        assert!(compact.hotspots[0].complexity.is_none());
        assert_eq!(standard.hotspots[0].change_frequency, Some(21));
        assert_eq!(evidence.hotspots[0].complexity, Some(34));

        for shaped in [&compact, &standard, &evidence] {
            assert_eq!(shaped.components.len(), 1);
            assert_eq!(shaped.components[0].id, "component-a");
            assert_eq!(shaped.components[0].kind, "crate");
            assert_eq!(shaped.components[0].name, "agent");
            assert_eq!(shaped.components[0].symbol_count, 17);
            assert_eq!(shaped.components[0].confidence, 940);
            assert_eq!(
                shaped.components[0].trust,
                TrustClassification::UntrustedRepositoryData
            );
            assert_eq!(shaped.connections, raw.connections);
            assert_eq!(shaped.hotspots[0].component_id, "component-a");
            assert_eq!(shaped.hotspots[0].fan_in, 8);
            assert_eq!(shaped.hotspots[0].fan_out, 13);
            assert_eq!(shaped.hotspots[0].score, 915);
            assert_eq!(shaped.views, raw.views);
        }
    }

    #[test]
    fn cycle_profiles_expand_evidence_without_changing_cycle_semantics() {
        let raw = ArchitectureCyclesData {
            components: vec![StronglyConnectedComponent {
                size: 2,
                members: vec!["component-a".to_owned(), "component-b".to_owned()],
                internal_edges: 3,
            }],
            cycles: vec![MinimalCycle {
                nodes: vec![
                    "component-a".to_owned(),
                    "component-b".to_owned(),
                    "component-a".to_owned(),
                ],
                edge_evidence: source_refs(6),
                confidence: 900,
            }],
            break_candidates: vec![CycleBreakCandidate {
                from: "component-a".to_owned(),
                to: "component-b".to_owned(),
                kind: RelationKind::Imports,
                break_cost: 420,
                source_refs: source_refs(6),
            }],
            explanation: None,
        };
        let (compact, standard, evidence) = shaped_profiles(&raw);

        assert_eq!(compact.cycles[0].edge_evidence.len(), 1);
        assert_eq!(standard.cycles[0].edge_evidence.len(), 4);
        assert_eq!(evidence.cycles[0].edge_evidence.len(), 6);
        assert_eq!(compact.break_candidates[0].source_refs.len(), 1);
        assert_eq!(standard.break_candidates[0].source_refs.len(), 4);
        assert_eq!(evidence.break_candidates[0].source_refs.len(), 6);
        assert!(compact.cycles[0].edge_evidence[0].line_hint().is_none());
        assert!(evidence.cycles[0].edge_evidence[0].line_hint().is_some());

        for shaped in [&compact, &standard, &evidence] {
            assert_eq!(shaped.components, raw.components);
            assert_eq!(shaped.cycles.len(), 1);
            assert_eq!(shaped.cycles[0].nodes, raw.cycles[0].nodes);
            assert_eq!(shaped.cycles[0].confidence, 900);
            assert_eq!(shaped.break_candidates.len(), 1);
            assert_eq!(shaped.break_candidates[0].from, "component-a");
            assert_eq!(shaped.break_candidates[0].to, "component-b");
            assert_eq!(shaped.break_candidates[0].kind, RelationKind::Imports);
            assert_eq!(shaped.break_candidates[0].break_cost, 420);
            for (actual, expected) in shaped.cycles[0]
                .edge_evidence
                .iter()
                .zip(&raw.cycles[0].edge_evidence)
            {
                assert_source_semantics(actual, expected);
            }
            for (actual, expected) in shaped.break_candidates[0]
                .source_refs
                .iter()
                .zip(&raw.break_candidates[0].source_refs)
            {
                assert_source_semantics(actual, expected);
            }
        }
    }

    #[test]
    fn plan_profiles_expand_detail_without_changing_plan_safety_or_order() {
        let target = SymbolId::from_bytes([50; 20]);
        let raw = PlanChangeData {
            plan: vec![ChangePlanStep {
                step: 1,
                action: "update the bounded response".to_owned(),
                targets: vec![target],
                depends_on: Vec::new(),
                risks: vec!["public-contract".to_owned()],
                verification: Some("run focused contract tests".to_owned()),
            }],
            affected_scope: PlanImpactSummary {
                affected_symbols: 3,
                affected_files: 2,
                risk_level: RiskLevel::Medium,
                touches_public_surface: true,
            },
            test_plan: vec![TestCandidate {
                test_id: "profile-contract".to_owned(),
                relevance: 980,
                why: (0..6).map(|index| format!("reason-{index}")).collect(),
                estimated_cost_ms: Some(250),
            }],
            open_decisions: vec![PlanDecision {
                question: "preserve wire compatibility".to_owned(),
                recommended_default: "yes".to_owned(),
            }],
            context_pack_request: ContextPackRequest {
                symbols: vec![target],
                files: vec![FileId::from_bytes([51; 20])],
            },
            explanation: None,
        };
        let (compact, standard, evidence) = shaped_profiles(&raw);

        assert_eq!(compact.test_plan[0].why.len(), 1);
        assert_eq!(standard.test_plan[0].why.len(), 4);
        assert_eq!(evidence.test_plan[0].why.len(), 6);
        assert!(compact.plan[0].verification.is_none());
        assert!(compact.test_plan[0].estimated_cost_ms.is_none());
        assert!(standard.plan[0].verification.is_some());
        assert_eq!(evidence.test_plan[0].estimated_cost_ms, Some(250));

        for shaped in [&compact, &standard, &evidence] {
            assert_eq!(shaped.plan.len(), 1);
            assert_eq!(shaped.plan[0].step, 1);
            assert_eq!(shaped.plan[0].action, raw.plan[0].action);
            assert_eq!(shaped.plan[0].targets, vec![target]);
            assert_eq!(shaped.plan[0].depends_on, raw.plan[0].depends_on);
            assert_eq!(shaped.plan[0].risks, raw.plan[0].risks);
            assert_eq!(shaped.affected_scope, raw.affected_scope);
            assert_eq!(shaped.test_plan.len(), 1);
            assert_eq!(shaped.test_plan[0].test_id, "profile-contract");
            assert_eq!(shaped.test_plan[0].relevance, 980);
            assert_eq!(shaped.open_decisions, raw.open_decisions);
            assert_eq!(shaped.context_pack_request, raw.context_pack_request);
        }
    }

    #[test]
    fn invariant_validation_allows_added_warnings_but_rejects_removal() {
        let original_warning = warning("profile-detail-omitted");
        let original = ResponseInvariants::new(
            ("repository", "generation"),
            ResultCompleteness::complete(),
            TrustClassification::UntrustedRepositoryData,
            vec![original_warning.clone()],
            ("next-cursor", "detail-handle"),
        );
        let with_added_warning = ResponseInvariants::new(
            ("repository", "generation"),
            ResultCompleteness::complete(),
            TrustClassification::UntrustedRepositoryData,
            vec![original_warning, warning("budget-detail-omitted")],
            ("next-cursor", "detail-handle"),
        );
        assert_eq!(original.validate_preserved(&with_added_warning), Ok(()));

        let without_warning = ResponseInvariants::new(
            ("repository", "generation"),
            ResultCompleteness::complete(),
            TrustClassification::UntrustedRepositoryData,
            Vec::new(),
            ("next-cursor", "detail-handle"),
        );
        assert_eq!(
            original.validate_preserved(&without_warning),
            Err(ProfileInvariantError::WarningRemoved)
        );
    }

    #[test]
    fn only_evidence_retains_source_line_hints() {
        let source_ref = SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(FileId::from_bytes([3; 20]), 4, 12).expect("valid source span"),
            ContentHash::from_bytes([4; 32]),
            Some(LineRange::new(2, 3).expect("valid line range")),
        );
        let mut compact = source_ref.clone();
        let mut standard = source_ref.clone();
        let mut evidence = source_ref;

        shape_source_ref(&mut compact, ResponseProfile::Compact);
        shape_source_ref(&mut standard, ResponseProfile::Standard);
        shape_source_ref(&mut evidence, ResponseProfile::Evidence);

        assert!(compact.line_hint().is_none());
        assert!(standard.line_hint().is_none());
        assert!(evidence.line_hint().is_some());
        assert_eq!(compact.span(), evidence.span());
        assert_eq!(compact.content_hash(), evidence.content_hash());
    }

    proptest! {
        #[test]
        fn profile_projection_preserves_semantics_for_bounded_test_rationales(
            rationale_count in 1_usize..=8,
            include_optional_fields in any::<bool>(),
        ) {
            let raw = test_selection_data(rationale_count, include_optional_fields);
            let mut compact = raw.clone();
            let mut standard = raw.clone();
            let mut evidence = raw;

            compact.shape(ResponseProfile::Compact);
            standard.shape(ResponseProfile::Standard);
            evidence.shape(ResponseProfile::Evidence);

            prop_assert!(compact.tests[0].why.len() <= standard.tests[0].why.len());
            prop_assert!(standard.tests[0].why.len() <= evidence.tests[0].why.len());
            prop_assert_eq!(&compact.tests[0].test_id, &standard.tests[0].test_id);
            prop_assert_eq!(&standard.tests[0].test_id, &evidence.tests[0].test_id);
            prop_assert_eq!(compact.tests[0].kind, standard.tests[0].kind);
            prop_assert_eq!(standard.tests[0].kind, evidence.tests[0].kind);
            prop_assert_eq!(compact.tests[0].score, standard.tests[0].score);
            prop_assert_eq!(standard.tests[0].score, evidence.tests[0].score);
            prop_assert_eq!(&compact.coverage_strategy, &standard.coverage_strategy);
            prop_assert_eq!(&standard.coverage_strategy, &evidence.coverage_strategy);
            prop_assert_eq!(&compact.gaps, &standard.gaps);
            prop_assert_eq!(&standard.gaps, &evidence.gaps);
        }
    }
}
