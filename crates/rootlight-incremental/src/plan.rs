//! Deterministic bounded fixed-point invalidation and artifact reuse planning.
//!
//! Missing dependency declarations or exhausted closure work escalate to an
//! explicit repository rebuild instead of permitting stale partial reuse.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use rootlight_cancel::Cancellation;
use serde::Serialize;

use crate::{
    AnalysisUnitId, ArtifactId, ChangeClass, ChangeSet, DependencyGraph, DependencySource,
    FactDomainSet, FactNode, GenerationSummary, INCREMENTAL_SCHEMA_VERSION, IncrementalError,
    InputKey, InputSnapshot, PassId, PlanningLimits, ResourceKind, model::check_count,
};

/// Why fine-grained invalidation escalated to a repository rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    /// A changed key had no declared dependent edge.
    MissingDependencyDeclaration,
    /// Fixed-point edge visits reached the configured closure ceiling.
    ClosureWorkExceeded,
}

/// Explicit conservative repository rebuild selected by the planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConservativeFallback {
    reason: FallbackReason,
}

impl ConservativeFallback {
    /// Returns the stable reason fine-grained reuse was disabled.
    #[must_use]
    pub const fn reason(self) -> FallbackReason {
        self.reason
    }
}

/// Artifact action selected by an invalidation plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDecisionKind {
    /// Reuse the immutable parent artifact.
    Reuse,
    /// Rebuild the artifact in the candidate generation.
    Rebuild,
}

/// One deterministic artifact reuse or rebuild decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDecision {
    artifact: ArtifactId,
    kind: ArtifactDecisionKind,
    reason: TraceReason,
}

impl ArtifactDecision {
    /// Returns the artifact identity.
    #[must_use]
    pub const fn artifact(&self) -> ArtifactId {
        self.artifact
    }

    /// Returns whether the artifact is reusable or must be rebuilt.
    #[must_use]
    pub const fn kind(&self) -> ArtifactDecisionKind {
        self.kind
    }

    /// Returns the stable decision reason.
    #[must_use]
    pub const fn reason(&self) -> &TraceReason {
        &self.reason
    }
}

/// Source-free target named by one invalidation trace entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TraceTarget {
    /// A changed generation input.
    Input(InputKey),
    /// A scoped fact-domain node.
    Fact(FactNode),
    /// A reusable immutable artifact.
    Artifact(ArtifactId),
    /// The complete invalidation plan.
    Plan,
}

/// Stable action recorded by an invalidation trace entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceAction {
    /// A complete generation input value changed.
    Changed,
    /// A fact node entered the fixed-point invalidation closure.
    Invalidated,
    /// An immutable artifact can be retained.
    Reused,
    /// An immutable artifact must be rebuilt.
    Rebuilt,
    /// Fine-grained reuse escalated to a repository rebuild.
    ConservativeFallback,
}

/// Stable source-free explanation for one trace action.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TraceReason {
    /// A complete input transitioned under this conservative class.
    InputTransition(ChangeClass),
    /// A declared pass dependency propagated invalidation.
    DependencyPass(PassId),
    /// The changed input had no declared dependent edge.
    MissingDependencyDeclaration,
    /// The configured fixed-point edge-visit budget was exhausted.
    ClosureWorkExceeded,
    /// One artifact output node is invalidated.
    ArtifactOutputInvalidated,
    /// One artifact's complete dependency fingerprint changed.
    ArtifactDependencyChanged(InputKey),
    /// Every artifact dependency and output remains reusable.
    CompleteDependencyMatch,
    /// A conservative repository fallback rebuilds this target.
    ConservativeRepositoryRebuild(FallbackReason),
}

/// One deterministic human- and machine-readable invalidation decision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraceEntry {
    target: TraceTarget,
    action: TraceAction,
    reason: TraceReason,
    via: Option<TraceTarget>,
}

impl TraceEntry {
    /// Returns the input, fact, artifact, or plan target.
    #[must_use]
    pub const fn target(&self) -> TraceTarget {
        self.target
    }

    /// Returns the stable action.
    #[must_use]
    pub const fn action(&self) -> TraceAction {
        self.action
    }

    /// Returns the stable reason.
    #[must_use]
    pub const fn reason(&self) -> &TraceReason {
        &self.reason
    }

    /// Returns the direct predecessor that propagated invalidation, when present.
    #[must_use]
    pub const fn via(&self) -> Option<TraceTarget> {
        self.via
    }
}

/// Versioned source-free invalidation trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvalidationTrace {
    version: String,
    entries: Vec<TraceEntry>,
}

impl InvalidationTrace {
    /// Returns the incremental schema version used by the trace.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns decisions in canonical target, action, and reason order.
    #[must_use]
    pub fn entries(&self) -> &[TraceEntry] {
        &self.entries
    }

    /// Serializes deterministic compact JSON.
    ///
    /// # Errors
    ///
    /// Returns [`IncrementalError::SerializeTrace`] on unexpected serializer
    /// failure.
    pub fn canonical_json(&self) -> Result<Vec<u8>, IncrementalError> {
        serde_json::to_vec(self).map_err(IncrementalError::SerializeTrace)
    }
}

impl fmt::Display for InvalidationTrace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "incremental trace {}", self.version)?;
        for entry in &self.entries {
            writeln!(
                formatter,
                "{:?} {:?}: {:?} via {:?}",
                entry.action, entry.target, entry.reason, entry.via
            )?;
        }
        Ok(())
    }
}

/// Complete bounded incremental invalidation and reuse plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidationPlan {
    changes: ChangeSet,
    invalidated_nodes: BTreeSet<FactNode>,
    reanalyze: BTreeSet<AnalysisUnitId>,
    rerun_domains: FactDomainSet,
    artifact_decisions: Vec<ArtifactDecision>,
    fallback: Option<ConservativeFallback>,
    trace: InvalidationTrace,
}

impl InvalidationPlan {
    /// Returns canonical complete input transitions.
    #[must_use]
    pub const fn changes(&self) -> &ChangeSet {
        &self.changes
    }

    /// Returns invalidated scoped fact nodes in canonical order.
    pub fn invalidated_nodes(&self) -> impl Iterator<Item = FactNode> + '_ {
        self.invalidated_nodes.iter().copied()
    }

    /// Returns analysis units that require work in canonical order.
    pub fn reanalyze(&self) -> impl Iterator<Item = AnalysisUnitId> + '_ {
        self.reanalyze.iter().copied()
    }

    /// Returns fact domains that must be rerun.
    #[must_use]
    pub const fn rerun_domains(&self) -> &FactDomainSet {
        &self.rerun_domains
    }

    /// Returns artifact decisions in canonical artifact order.
    #[must_use]
    pub fn artifact_decisions(&self) -> &[ArtifactDecision] {
        &self.artifact_decisions
    }

    /// Returns an explicit repository rebuild fallback, when selected.
    #[must_use]
    pub const fn fallback(&self) -> Option<ConservativeFallback> {
        self.fallback
    }

    /// Returns the complete source-free explanation.
    #[must_use]
    pub const fn trace(&self) -> &InvalidationTrace {
        &self.trace
    }
}

/// Computes dependency-directed invalidation and safe artifact reuse.
///
/// The function compares complete parent and current input snapshots itself,
/// walks declared dependencies to a fixed point, and chooses an explicit
/// repository rebuild if a changed input is undeclared or closure work exceeds
/// its ceiling.
///
/// # Errors
///
/// Returns a malformed artifact-graph, resource-limit, serialization-independent,
/// or cancellation error. No active generation is mutated.
pub fn plan_invalidation(
    parent: &GenerationSummary,
    current: &InputSnapshot,
    graph: &DependencyGraph,
    limits: PlanningLimits,
    cancellation: &Cancellation,
) -> Result<InvalidationPlan, IncrementalError> {
    check_count(ResourceKind::Inputs, current.len(), limits.max_inputs)?;
    for artifact in parent.artifacts() {
        cancellation.check()?;
        for node in artifact.outputs() {
            if !graph.contains_node(node) {
                return Err(IncrementalError::ArtifactUnknownOutput {
                    artifact: artifact.id(),
                    node,
                });
            }
        }
    }

    let changes = parent.inputs().changes_to(current, limits, cancellation)?;
    let mut invalidated = BTreeSet::new();
    let mut frontier = BTreeSet::new();
    let mut causes: BTreeMap<FactNode, (DependencySource, PassId)> = BTreeMap::new();
    let mut fallback = None;
    let mut closure_work = 0_usize;

    for change in changes.changes() {
        cancellation.check()?;
        if !graph.has_input_dependency(change.key()) {
            fallback = Some(ConservativeFallback {
                reason: FallbackReason::MissingDependencyDeclaration,
            });
            break;
        }
        for (target, pass) in graph.input_dependents(change.key()) {
            cancellation.check()?;
            if !charge_closure_work(&mut closure_work, limits) {
                fallback = Some(ConservativeFallback {
                    reason: FallbackReason::ClosureWorkExceeded,
                });
                break;
            }
            if invalidated.insert(target) {
                frontier.insert(target);
                causes.insert(
                    target,
                    (DependencySource::Input(change.key()), pass.clone()),
                );
            }
        }
        if fallback.is_some() {
            break;
        }
    }

    while fallback.is_none()
        && let Some(source) = frontier.pop_first()
    {
        cancellation.check()?;
        for (target, pass) in graph.fact_dependents(source) {
            cancellation.check()?;
            if !charge_closure_work(&mut closure_work, limits) {
                fallback = Some(ConservativeFallback {
                    reason: FallbackReason::ClosureWorkExceeded,
                });
                break;
            }
            if invalidated.insert(target) {
                frontier.insert(target);
                causes.insert(target, (DependencySource::Fact(source), pass.clone()));
            }
        }
    }

    if fallback.is_some() {
        invalidated.clear();
        for node in graph.nodes() {
            cancellation.check()?;
            invalidated.insert(node);
        }
    }

    let mut reanalyze = BTreeSet::new();
    let mut rerun_domains = FactDomainSet::default();
    for node in &invalidated {
        cancellation.check()?;
        reanalyze.insert(node.unit());
        rerun_domains.insert(node.domain());
    }

    let mut artifact_decisions = Vec::new();
    for artifact in parent.artifacts() {
        cancellation.check()?;
        let (kind, reason) = match fallback {
            Some(fallback) => (
                ArtifactDecisionKind::Rebuild,
                TraceReason::ConservativeRepositoryRebuild(fallback.reason()),
            ),
            None => {
                if artifact.outputs().any(|node| invalidated.contains(&node)) {
                    (
                        ArtifactDecisionKind::Rebuild,
                        TraceReason::ArtifactOutputInvalidated,
                    )
                } else if let Some(changed_key) = artifact
                    .dependencies()
                    .find(|dependency| current.value(dependency.key()) != Some(dependency.value()))
                    .map(|dependency| dependency.key())
                {
                    (
                        ArtifactDecisionKind::Rebuild,
                        TraceReason::ArtifactDependencyChanged(changed_key),
                    )
                } else {
                    (
                        ArtifactDecisionKind::Reuse,
                        TraceReason::CompleteDependencyMatch,
                    )
                }
            }
        };
        artifact_decisions.push(ArtifactDecision {
            artifact: artifact.id(),
            kind,
            reason,
        });
    }

    let trace = build_trace(
        &changes,
        &invalidated,
        &causes,
        &artifact_decisions,
        fallback,
        limits,
        cancellation,
    )?;
    Ok(InvalidationPlan {
        changes,
        invalidated_nodes: invalidated,
        reanalyze,
        rerun_domains,
        artifact_decisions,
        fallback,
        trace,
    })
}

fn charge_closure_work(work: &mut usize, limits: PlanningLimits) -> bool {
    *work = work.saturating_add(1);
    *work <= limits.max_closure_work
}

fn build_trace(
    changes: &ChangeSet,
    invalidated: &BTreeSet<FactNode>,
    causes: &BTreeMap<FactNode, (DependencySource, PassId)>,
    artifact_decisions: &[ArtifactDecision],
    fallback: Option<ConservativeFallback>,
    limits: PlanningLimits,
    cancellation: &Cancellation,
) -> Result<InvalidationTrace, IncrementalError> {
    let entry_count = changes
        .changes()
        .len()
        .saturating_add(invalidated.len())
        .saturating_add(artifact_decisions.len())
        .saturating_add(usize::from(fallback.is_some()));
    check_count(
        ResourceKind::TraceEntries,
        entry_count,
        limits.max_trace_entries,
    )?;

    let mut entries = Vec::with_capacity(entry_count);
    for change in changes.changes() {
        cancellation.check()?;
        entries.push(TraceEntry {
            target: TraceTarget::Input(change.key()),
            action: TraceAction::Changed,
            reason: TraceReason::InputTransition(change.class()),
            via: None,
        });
    }
    if let Some(fallback) = fallback {
        entries.push(TraceEntry {
            target: TraceTarget::Plan,
            action: TraceAction::ConservativeFallback,
            reason: match fallback.reason() {
                FallbackReason::MissingDependencyDeclaration => {
                    TraceReason::MissingDependencyDeclaration
                }
                FallbackReason::ClosureWorkExceeded => TraceReason::ClosureWorkExceeded,
            },
            via: None,
        });
    }
    for node in invalidated {
        cancellation.check()?;
        let (reason, via) = match fallback {
            Some(fallback) => (
                TraceReason::ConservativeRepositoryRebuild(fallback.reason()),
                Some(TraceTarget::Plan),
            ),
            None => {
                let Some((source, pass)) = causes.get(node) else {
                    // A non-fallback node is inserted only together with its cause.
                    return Err(IncrementalError::UnknownFactNode { node: *node });
                };
                (
                    TraceReason::DependencyPass(pass.clone()),
                    Some(match source {
                        DependencySource::Input(key) => TraceTarget::Input(*key),
                        DependencySource::Fact(node) => TraceTarget::Fact(*node),
                    }),
                )
            }
        };
        entries.push(TraceEntry {
            target: TraceTarget::Fact(*node),
            action: TraceAction::Invalidated,
            reason,
            via,
        });
    }
    for decision in artifact_decisions {
        cancellation.check()?;
        entries.push(TraceEntry {
            target: TraceTarget::Artifact(decision.artifact()),
            action: match decision.kind() {
                ArtifactDecisionKind::Reuse => TraceAction::Reused,
                ArtifactDecisionKind::Rebuild => TraceAction::Rebuilt,
            },
            reason: decision.reason().clone(),
            via: None,
        });
    }
    entries.sort();

    Ok(InvalidationTrace {
        version: INCREMENTAL_SCHEMA_VERSION.to_owned(),
        entries,
    })
}
