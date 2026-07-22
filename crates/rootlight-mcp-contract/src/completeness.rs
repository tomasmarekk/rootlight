//! Transport-neutral execution-completeness and continuation semantics.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Authoritative execution state for one bounded analytical result.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CompletenessState {
    /// The producer authoritatively completed the supported execution domain.
    Complete,
    /// A known resource limit stopped an otherwise supported execution.
    Truncated,
    /// A known part of the requested semantic domain is unsupported.
    UnsupportedPartial,
    /// The producer cannot establish what portion of the domain was evaluated.
    Indeterminate,
}

/// Stable resource family that limited an analytical result.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LimitingResourceKind {
    /// Logical row limit.
    Rows,
    /// Traversed edge limit.
    Edges,
    /// Returned result limit.
    Results,
    /// Traversal depth limit.
    Depth,
    /// Returned path limit.
    Paths,
    /// Returned source-byte limit.
    SourceBytes,
    /// Serialized response-byte limit.
    ResponseBytes,
    /// Owned response-memory limit.
    MemoryBytes,
    /// Monotonic deadline.
    Deadline,
    /// Estimated output-token limit.
    EstimatedTokens,
    /// Cooperative cancellation.
    Cancellation,
    /// Unavailable requested capability.
    Capability,
    /// Incomplete index or semantic coverage.
    Coverage,
    /// Requested or effective page-size limit.
    PageSize,
}

/// One source-free limiting-resource observation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct LimitingResource {
    /// Stable limiting-resource family.
    pub kind: LimitingResourceKind,
    /// Effective ceiling, when the producer measured it.
    pub limit: Option<u64>,
    /// Observed value at the stop boundary, when available.
    pub observed: Option<u64>,
}

impl LimitingResource {
    /// Creates a resource observation without optional measurements.
    #[must_use]
    pub const fn kind(kind: LimitingResourceKind) -> Self {
        Self {
            kind,
            limit: None,
            observed: None,
        }
    }
}

/// Whether an incomplete result exposes a safe continuation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationAvailability {
    /// Continuation does not apply to a complete or atomic result.
    NotApplicable,
    /// A checked continuation is present at the public boundary.
    Available,
    /// The result is incomplete and cannot be continued directly.
    Unavailable,
}

/// Typed source-free follow-up guidance for an incomplete result.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationGuidance {
    /// Continue with the authenticated cursor.
    UseCursor,
    /// Narrow the requested scope.
    NarrowScope,
    /// Split the request into independently bounded requests.
    SplitRequest,
    /// Reduce traversal depth.
    ReduceDepth,
    /// Reduce the relation projection.
    ReduceRelations,
    /// Request source separately from analytical evidence.
    RequestSource,
    /// Increase a caller-controlled budget without exceeding server policy.
    IncreaseBudgetWithinLimit,
    /// Refresh or expand indexed coverage.
    RefreshCoverage,
    /// The unsupported or unknown portion has no continuation.
    UnsupportedNoContinuation,
}

/// Checked completeness, limiting resources, and continuation semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, try_from = "UncheckedResultCompleteness")]
pub struct ResultCompleteness {
    /// Authoritative execution state.
    pub state: CompletenessState,
    /// Deterministically ordered unique limiting resources.
    #[schemars(length(max = 14))]
    pub limiting_resources: Vec<LimitingResource>,
    /// Continuation availability at the current boundary.
    pub continuation: ContinuationAvailability,
    /// Deterministically ordered unique source-free guidance.
    #[schemars(length(max = 9))]
    pub guidance: Vec<ContinuationGuidance>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UncheckedResultCompleteness {
    state: CompletenessState,
    #[schemars(length(max = 14))]
    limiting_resources: Vec<LimitingResource>,
    continuation: ContinuationAvailability,
    #[schemars(length(max = 9))]
    guidance: Vec<ContinuationGuidance>,
}

impl TryFrom<UncheckedResultCompleteness> for ResultCompleteness {
    type Error = CompletenessError;

    fn try_from(value: UncheckedResultCompleteness) -> Result<Self, Self::Error> {
        Self::new(
            value.state,
            value.limiting_resources,
            value.continuation,
            value.guidance,
        )
    }
}

impl ResultCompleteness {
    /// Creates and validates one completeness record.
    ///
    /// # Errors
    ///
    /// Returns [`CompletenessError`] when state, resources, continuation, or
    /// guidance could allow an incomplete result to masquerade as complete.
    pub fn new(
        state: CompletenessState,
        limiting_resources: Vec<LimitingResource>,
        continuation: ContinuationAvailability,
        guidance: Vec<ContinuationGuidance>,
    ) -> Result<Self, CompletenessError> {
        if limiting_resources.len() > 14 {
            return Err(CompletenessError::TooManyResources);
        }
        if guidance.len() > 9 {
            return Err(CompletenessError::TooMuchGuidance);
        }
        let mut value = Self {
            state,
            limiting_resources,
            continuation,
            guidance,
        };
        value.limiting_resources.sort_unstable();
        value.guidance.sort_unstable();
        value.guidance.dedup();
        value.validate()?;
        Ok(value)
    }

    /// Creates an authoritative complete result.
    #[must_use]
    pub const fn complete() -> Self {
        Self {
            state: CompletenessState::Complete,
            limiting_resources: Vec::new(),
            continuation: ContinuationAvailability::NotApplicable,
            guidance: Vec::new(),
        }
    }

    /// Creates the fail-closed representation of an older or unknown producer.
    #[must_use]
    pub fn indeterminate() -> Self {
        Self {
            state: CompletenessState::Indeterminate,
            limiting_resources: Vec::new(),
            continuation: ContinuationAvailability::Unavailable,
            guidance: vec![ContinuationGuidance::UnsupportedNoContinuation],
        }
    }

    /// Merges two records using the weakest authoritative execution state.
    ///
    /// # Errors
    ///
    /// Returns [`CompletenessError`] if the merged continuation semantics are
    /// inconsistent with the resulting state.
    pub fn merge(&self, other: &Self) -> Result<Self, CompletenessError> {
        let state = self.state.max(other.state);
        let limiting_resources = self
            .limiting_resources
            .iter()
            .chain(&other.limiting_resources)
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let guidance = self
            .guidance
            .iter()
            .chain(&other.guidance)
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|guidance| {
                *guidance != ContinuationGuidance::UseCursor
                    || continuation_is_available(self, other, state)
            })
            .collect();
        let continuation = if state == CompletenessState::Complete {
            ContinuationAvailability::NotApplicable
        } else if self.continuation == ContinuationAvailability::Unavailable
            || other.continuation == ContinuationAvailability::Unavailable
        {
            ContinuationAvailability::Unavailable
        } else if self.continuation == ContinuationAvailability::Available
            || other.continuation == ContinuationAvailability::Available
        {
            ContinuationAvailability::Available
        } else {
            ContinuationAvailability::Unavailable
        };
        Self::new(state, limiting_resources, continuation, guidance)
    }

    fn validate(&self) -> Result<(), CompletenessError> {
        if self.state == CompletenessState::Complete {
            if !self.limiting_resources.is_empty()
                || self.continuation != ContinuationAvailability::NotApplicable
                || !self.guidance.is_empty()
            {
                return Err(CompletenessError::CompleteHasCaveats);
            }
            return Ok(());
        }
        if self
            .limiting_resources
            .windows(2)
            .any(|pair| pair[0].kind == pair[1].kind)
        {
            return Err(CompletenessError::DuplicateResource);
        }
        if self.limiting_resources.iter().any(|resource| {
            resource
                .limit
                .zip(resource.observed)
                .is_some_and(|(limit, observed)| observed < limit)
        }) {
            return Err(CompletenessError::ObservationBelowLimit);
        }
        if self.state == CompletenessState::Truncated && self.limiting_resources.is_empty() {
            return Err(CompletenessError::TruncatedWithoutResource);
        }
        if self.state == CompletenessState::UnsupportedPartial
            && !self.limiting_resources.iter().any(|resource| {
                matches!(
                    resource.kind,
                    LimitingResourceKind::Capability | LimitingResourceKind::Coverage
                )
            })
        {
            return Err(CompletenessError::UnsupportedWithoutBoundary);
        }
        if self.continuation == ContinuationAvailability::NotApplicable {
            return Err(CompletenessError::IncompleteWithoutDisposition);
        }
        if self.continuation == ContinuationAvailability::Available
            && !self.guidance.contains(&ContinuationGuidance::UseCursor)
        {
            return Err(CompletenessError::AvailableWithoutCursorGuidance);
        }
        if self.continuation == ContinuationAvailability::Unavailable && self.guidance.is_empty() {
            return Err(CompletenessError::UnavailableWithoutGuidance);
        }
        Ok(())
    }
}

/// Invalid completeness composition that could produce an unsafe claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CompletenessError {
    /// A complete result retained a limit, continuation, or caveat.
    #[error("complete result contains an incomplete-result caveat")]
    CompleteHasCaveats,
    /// More than one observation used the same limiting-resource family.
    #[error("completeness contains a duplicate limiting resource")]
    DuplicateResource,
    /// The bounded completeness record exceeded the resource-family count.
    #[error("completeness contains too many limiting resources")]
    TooManyResources,
    /// The bounded completeness record exceeded the guidance count.
    #[error("completeness contains too much continuation guidance")]
    TooMuchGuidance,
    /// A resource observation stopped below its declared effective ceiling.
    #[error("limiting resource observation is below its limit")]
    ObservationBelowLimit,
    /// A truncated result did not identify its stopping resource.
    #[error("truncated result has no limiting resource")]
    TruncatedWithoutResource,
    /// An unsupported result did not identify capability or coverage scope.
    #[error("unsupported partial result has no capability or coverage boundary")]
    UnsupportedWithoutBoundary,
    /// An incomplete result did not state whether it can be continued.
    #[error("incomplete result has no continuation disposition")]
    IncompleteWithoutDisposition,
    /// An available continuation omitted stable cursor guidance.
    #[error("available continuation has no cursor guidance")]
    AvailableWithoutCursorGuidance,
    /// An unavailable continuation omitted safe follow-up guidance.
    #[error("unavailable continuation has no follow-up guidance")]
    UnavailableWithoutGuidance,
}

fn continuation_is_available(
    left: &ResultCompleteness,
    right: &ResultCompleteness,
    state: CompletenessState,
) -> bool {
    state != CompletenessState::Complete
        && left.continuation != ContinuationAvailability::Unavailable
        && right.continuation != ContinuationAvailability::Unavailable
        && (left.continuation == ContinuationAvailability::Available
            || right.continuation == ContinuationAvailability::Available)
}

#[cfg(test)]
mod tests {
    use super::{
        CompletenessError, CompletenessState, ContinuationAvailability, ContinuationGuidance,
        LimitingResource, LimitingResourceKind, ResultCompleteness,
    };

    #[test]
    fn validation_rejects_optimistic_or_unactionable_records() {
        assert_eq!(
            ResultCompleteness::new(
                CompletenessState::Truncated,
                Vec::new(),
                ContinuationAvailability::Unavailable,
                vec![ContinuationGuidance::NarrowScope],
            ),
            Err(CompletenessError::TruncatedWithoutResource)
        );
        assert_eq!(
            ResultCompleteness::new(
                CompletenessState::UnsupportedPartial,
                vec![LimitingResource::kind(LimitingResourceKind::Rows)],
                ContinuationAvailability::Unavailable,
                vec![ContinuationGuidance::NarrowScope],
            ),
            Err(CompletenessError::UnsupportedWithoutBoundary)
        );
        assert_eq!(
            ResultCompleteness::new(
                CompletenessState::Indeterminate,
                Vec::new(),
                ContinuationAvailability::Unavailable,
                Vec::new(),
            ),
            Err(CompletenessError::UnavailableWithoutGuidance)
        );
    }

    #[test]
    fn merge_uses_the_weakest_state_and_stable_unions() {
        let truncated = ResultCompleteness::new(
            CompletenessState::Truncated,
            vec![LimitingResource::kind(LimitingResourceKind::Rows)],
            ContinuationAvailability::Available,
            vec![ContinuationGuidance::UseCursor],
        )
        .expect("truncated page is valid");
        let unsupported = ResultCompleteness::new(
            CompletenessState::UnsupportedPartial,
            vec![LimitingResource::kind(LimitingResourceKind::Coverage)],
            ContinuationAvailability::Unavailable,
            vec![ContinuationGuidance::RefreshCoverage],
        )
        .expect("coverage partiality is valid");

        let merged = truncated
            .merge(&unsupported)
            .expect("weakest-state merge is valid");
        assert_eq!(merged.state, CompletenessState::UnsupportedPartial);
        assert_eq!(merged.continuation, ContinuationAvailability::Unavailable);
        assert_eq!(
            merged.limiting_resources,
            vec![
                LimitingResource::kind(LimitingResourceKind::Rows),
                LimitingResource::kind(LimitingResourceKind::Coverage),
            ]
        );
        assert_eq!(merged.guidance, vec![ContinuationGuidance::RefreshCoverage]);
    }

    #[test]
    fn duplicate_resource_families_are_rejected() {
        assert_eq!(
            ResultCompleteness::new(
                CompletenessState::Truncated,
                vec![
                    LimitingResource {
                        kind: LimitingResourceKind::Rows,
                        limit: Some(10),
                        observed: Some(10),
                    },
                    LimitingResource {
                        kind: LimitingResourceKind::Rows,
                        limit: Some(20),
                        observed: Some(20),
                    },
                ],
                ContinuationAvailability::Unavailable,
                vec![ContinuationGuidance::NarrowScope],
            ),
            Err(CompletenessError::DuplicateResource)
        );
    }
}
