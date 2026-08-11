//! Bounded impact, flow, context, plan, and migration traversals.
//!
//! Every row comes from an immutable link overlay, retains both endpoint
//! generations, and is paged by a snapshot-bound continuation.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{ContentHash, RepositoryId};
use rootlight_ir::Confidence;
use serde::Serialize;

use crate::{
    LinkCaveat, LinkKind, LinkOverlay, LinkOverlayId, ServiceKey, SnapshotFailure,
    WorkspaceFactRef, WorkspaceSnapshot, WorkspaceSnapshotId, identity::identity_hash,
};

const WORKFLOW_SCHEMA_VERSION: u16 = 1;
const HARD_MAX_REPOSITORIES: usize = 1_024;
const HARD_MAX_ROWS: usize = 100_000;
const HARD_MAX_EDGES: usize = 1_000_000;
const HARD_MAX_DEPTH: usize = 32;
const HARD_MAX_FANOUT: usize = 1_024;
const HARD_MAX_ROWS_PER_REPOSITORY: usize = 100_000;
const HARD_MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const HARD_MAX_JSON_BYTES: usize = 32 * 1024 * 1024;
const HARD_MAX_TOKENS: usize = 8_000_000;
const HARD_MAX_SEEDS: usize = 256;

/// Closed workspace workflow family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WorkflowKind {
    /// Traverse providers toward consumers that may be affected.
    Impact,
    /// Traverse consumers toward provider dependencies.
    Flow,
    /// Traverse both directions for context assembly.
    Context,
    /// Traverse both directions for an ordered change plan.
    Plan,
    /// Traverse both directions for migration scope.
    Migration,
}

/// Aggregate and per-repository workflow budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowBudget {
    max_repositories: usize,
    max_rows: usize,
    max_edges: usize,
    max_depth: usize,
    max_fanout: usize,
    max_rows_per_repository: usize,
    max_source_bytes: usize,
    max_json_bytes: usize,
    max_tokens: usize,
}

impl WorkflowBudget {
    /// Creates traversal limits with conservative response ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::InvalidBudget`] when any traversal value is
    /// zero or exceeds its hard process ceiling.
    pub fn new(
        max_repositories: usize,
        max_rows: usize,
        max_edges: usize,
        max_depth: usize,
        max_fanout: usize,
    ) -> Result<Self, WorkflowError> {
        let budget = Self {
            max_repositories,
            max_rows,
            max_edges,
            max_depth,
            max_fanout,
            max_rows_per_repository: max_rows,
            max_source_bytes: 0,
            max_json_bytes: 4 * 1024 * 1024,
            max_tokens: 1_000_000,
        };
        budget.validate()?;
        Ok(budget)
    }

    /// Replaces per-repository and serialized response ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::InvalidBudget`] when any value exceeds its hard
    /// process ceiling. A zero source-byte budget is valid because workflows do
    /// not read source content.
    pub fn with_response_limits(
        mut self,
        max_rows_per_repository: usize,
        max_source_bytes: usize,
        max_json_bytes: usize,
        max_tokens: usize,
    ) -> Result<Self, WorkflowError> {
        self.max_rows_per_repository = max_rows_per_repository;
        self.max_source_bytes = max_source_bytes;
        self.max_json_bytes = max_json_bytes;
        self.max_tokens = max_tokens;
        self.validate()?;
        Ok(self)
    }

    fn validate(self) -> Result<(), WorkflowError> {
        if self.max_repositories == 0
            || self.max_repositories > HARD_MAX_REPOSITORIES
            || self.max_rows == 0
            || self.max_rows > HARD_MAX_ROWS
            || self.max_edges == 0
            || self.max_edges > HARD_MAX_EDGES
            || self.max_depth == 0
            || self.max_depth > HARD_MAX_DEPTH
            || self.max_fanout == 0
            || self.max_fanout > HARD_MAX_FANOUT
            || self.max_rows_per_repository == 0
            || self.max_rows_per_repository > HARD_MAX_ROWS_PER_REPOSITORY
            || self.max_source_bytes > HARD_MAX_SOURCE_BYTES
            || self.max_json_bytes == 0
            || self.max_json_bytes > HARD_MAX_JSON_BYTES
            || self.max_tokens == 0
            || self.max_tokens > HARD_MAX_TOKENS
        {
            return Err(WorkflowError::InvalidBudget);
        }
        Ok(())
    }
}

impl Default for WorkflowBudget {
    fn default() -> Self {
        Self {
            max_repositories: 128,
            max_rows: 1_000,
            max_edges: 20_000,
            max_depth: 8,
            max_fanout: 64,
            max_rows_per_repository: 500,
            max_source_bytes: 0,
            max_json_bytes: 4 * 1024 * 1024,
            max_tokens: 1_000_000,
        }
    }
}

/// Opaque stateless continuation bound to immutable workflow inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowContinuation {
    snapshot: WorkspaceSnapshotId,
    overlay: LinkOverlayId,
    fingerprint: ContentHash,
    offset: usize,
}

impl WorkflowContinuation {
    /// Returns the number of canonical reachable edges already emitted.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset
    }
}

/// Declarative workflow request.
#[derive(Debug, Clone)]
pub struct WorkflowRequest {
    kind: WorkflowKind,
    seeds: Vec<WorkspaceFactRef>,
    budget: WorkflowBudget,
    continuation: Option<WorkflowContinuation>,
}

impl WorkflowRequest {
    /// Creates an empty request.
    #[must_use]
    pub const fn new(kind: WorkflowKind, budget: WorkflowBudget) -> Self {
        Self {
            kind,
            seeds: Vec::new(),
            budget,
            continuation: None,
        }
    }

    /// Adds one exact seed endpoint.
    #[must_use]
    pub fn with_seed(mut self, seed: WorkspaceFactRef) -> Self {
        self.seeds.push(seed);
        self
    }

    /// Resumes from a continuation returned by an earlier page.
    #[must_use]
    pub const fn with_continuation(mut self, continuation: WorkflowContinuation) -> Self {
        self.continuation = Some(continuation);
        self
    }
}

/// One explicit traversed candidate edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEdge {
    id: ContentHash,
    link: ContentHash,
    kind: LinkKind,
    key: ServiceKey,
    from: WorkspaceFactRef,
    to: WorkspaceFactRef,
    confidence: Confidence,
    caveats: Vec<LinkCaveat>,
}

impl WorkflowEdge {
    /// Returns the deterministic workflow-edge identity.
    #[must_use]
    pub const fn id(&self) -> ContentHash {
        self.id
    }

    /// Returns the exact source endpoint.
    #[must_use]
    pub const fn from(&self) -> WorkspaceFactRef {
        self.from
    }

    /// Returns the exact target endpoint.
    #[must_use]
    pub const fn to(&self) -> WorkspaceFactRef {
        self.to
    }

    /// Returns the relation family.
    #[must_use]
    pub const fn kind(&self) -> LinkKind {
        self.kind
    }
}

/// Rows charged to one repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryUsage {
    repository: RepositoryId,
    rows: usize,
}

impl RepositoryUsage {
    /// Returns the repository identity.
    #[must_use]
    pub const fn repository(self) -> RepositoryId {
        self.repository
    }

    /// Returns the number of rows involving this repository.
    #[must_use]
    pub const fn rows(self) -> usize {
        self.rows
    }
}

/// Bounded workflow page with explicit partial failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowResult {
    schema_version: u16,
    kind: WorkflowKind,
    snapshot: WorkspaceSnapshotId,
    overlay: LinkOverlayId,
    edges_scanned: usize,
    rows: Vec<WorkflowEdge>,
    repository_usage: Vec<RepositoryUsage>,
    repository_failures: Vec<SnapshotFailure>,
    source_bytes: usize,
    estimated_json_bytes: usize,
    estimated_tokens: usize,
    truncated: bool,
    continuation: Option<WorkflowContinuation>,
}

impl WorkflowResult {
    /// Returns rows in deterministic traversal order.
    #[must_use]
    pub fn rows(&self) -> &[WorkflowEdge] {
        &self.rows
    }

    /// Returns per-repository row accounting.
    #[must_use]
    pub fn repository_usage(&self) -> &[RepositoryUsage] {
        &self.repository_usage
    }

    /// Returns snapshot omissions that prevent exhaustive workspace claims.
    #[must_use]
    pub fn repository_failures(&self) -> &[SnapshotFailure] {
        &self.repository_failures
    }

    /// Returns source bytes read while producing this page.
    ///
    /// Workspace workflows currently operate on immutable facts only, so this
    /// value is always zero and remains explicit in evidence artifacts.
    #[must_use]
    pub const fn source_bytes(&self) -> usize {
        self.source_bytes
    }

    /// Returns the number of unique reachable edges examined.
    #[must_use]
    pub const fn edges_scanned(&self) -> usize {
        self.edges_scanned
    }

    /// Returns the conservative serialized-size estimate.
    #[must_use]
    pub const fn estimated_json_bytes(&self) -> usize {
        self.estimated_json_bytes
    }

    /// Returns the conservative token estimate.
    #[must_use]
    pub const fn estimated_tokens(&self) -> usize {
        self.estimated_tokens
    }

    /// Reports whether any traversal or response budget truncated the result.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// Returns a stateless continuation for a later page.
    #[must_use]
    pub const fn continuation(&self) -> Option<WorkflowContinuation> {
        self.continuation
    }
}

#[derive(Debug, Clone)]
struct DirectedEdge {
    edge: WorkflowEdge,
}

#[derive(Debug)]
struct DirectedEdges {
    edges: Vec<DirectedEdge>,
    truncated: bool,
}

/// Executes a deterministic traversal without consulting live generations.
///
/// # Errors
///
/// Returns [`WorkflowError`] for inconsistent immutable inputs, invalid seeds
/// or continuation, resource accounting, response metadata overflow, or
/// cancellation.
pub fn execute_workflow(
    snapshot: &WorkspaceSnapshot,
    overlay: &LinkOverlay,
    mut request: WorkflowRequest,
    cancellation: &Cancellation,
) -> Result<WorkflowResult, WorkflowError> {
    cancellation.check()?;
    request.budget.validate()?;
    if overlay.snapshot() != snapshot.id() {
        return Err(WorkflowError::OverlayMismatch);
    }
    if request.seeds.is_empty() {
        return Err(WorkflowError::EmptySeeds);
    }
    if request.seeds.len() > HARD_MAX_SEEDS {
        return Err(WorkflowError::SeedLimit);
    }
    request.seeds.sort();
    request.seeds.dedup();
    for seed in &request.seeds {
        if snapshot.generation_for(seed.repository()) != Some(seed.generation()) {
            return Err(WorkflowError::SeedOutsideSnapshot);
        }
    }
    let fingerprint = workflow_fingerprint(request.kind, &request.seeds, request.budget.max_depth);
    let offset = match request.continuation {
        Some(continuation)
            if continuation.snapshot == snapshot.id()
                && continuation.overlay == overlay.id()
                && continuation.fingerprint == fingerprint
                && continuation.offset <= HARD_MAX_EDGES =>
        {
            continuation.offset
        }
        Some(_) => return Err(WorkflowError::ContinuationMismatch),
        None => 0,
    };
    let directed = directed_edges(
        request.kind,
        overlay,
        request.budget.max_edges,
        cancellation,
    )?;
    let adjacency = adjacency(&directed.edges)?;
    let metadata_bytes = serde_json::to_vec(snapshot.failures())
        .map_err(|_| WorkflowError::Serialization)?
        .len()
        .saturating_add(1_024);
    if metadata_bytes > request.budget.max_json_bytes
        || token_estimate(metadata_bytes) > request.budget.max_tokens
    {
        return Err(WorkflowError::ResponseLimit);
    }

    let mut queue = request
        .seeds
        .iter()
        .copied()
        .map(|seed| (seed, 0_usize))
        .collect::<VecDeque<_>>();
    let mut visited_nodes = request.seeds.iter().copied().collect::<BTreeSet<_>>();
    let mut visited_edges = BTreeSet::new();
    let mut repositories = request
        .seeds
        .iter()
        .map(|seed| seed.repository())
        .collect::<BTreeSet<_>>();
    let mut usage = BTreeMap::<RepositoryId, usize>::new();
    let mut rows = Vec::new();
    let mut estimated_json_bytes = metadata_bytes;
    let mut ordinal = 0_usize;
    let result_incomplete = !snapshot.is_complete();
    let mut traversal_truncated = directed.truncated;
    let mut continuable = false;
    let mut continuation_offset = offset;

    'traversal: while let Some((node, depth)) = queue.pop_front() {
        cancellation.check()?;
        if depth >= request.budget.max_depth {
            continue;
        }
        let Some(outbound) = adjacency.get(&node) else {
            continue;
        };
        if outbound.len() > request.budget.max_fanout {
            traversal_truncated = true;
        }
        for edge_index in outbound.iter().copied().take(request.budget.max_fanout) {
            if !visited_edges.insert(edge_index) {
                continue;
            }
            ordinal = ordinal.checked_add(1).ok_or(WorkflowError::Accounting)?;
            if ordinal > request.budget.max_edges {
                traversal_truncated = true;
                break 'traversal;
            }
            let edge = &directed
                .edges
                .get(edge_index)
                .ok_or(WorkflowError::Accounting)?
                .edge;
            repositories.insert(edge.from.repository());
            repositories.insert(edge.to.repository());
            if repositories.len() > request.budget.max_repositories {
                traversal_truncated = true;
                break 'traversal;
            }
            if visited_nodes.insert(edge.to) {
                queue.push_back((edge.to, depth.saturating_add(1)));
            }
            if ordinal <= offset {
                continuation_offset = ordinal;
                continue;
            }
            if rows.len() >= request.budget.max_rows {
                traversal_truncated = true;
                continuable = true;
                break 'traversal;
            }
            let from_rows = usage.get(&edge.from.repository()).copied().unwrap_or(0);
            let to_rows = usage.get(&edge.to.repository()).copied().unwrap_or(0);
            if from_rows >= request.budget.max_rows_per_repository
                || to_rows >= request.budget.max_rows_per_repository
            {
                traversal_truncated = true;
                continuation_offset = ordinal;
                continue;
            }
            let row_bytes = serde_json::to_vec(edge)
                .map_err(|_| WorkflowError::Serialization)?
                .len()
                .saturating_add(1);
            let next_json_bytes = estimated_json_bytes.saturating_add(row_bytes);
            if next_json_bytes > request.budget.max_json_bytes
                || token_estimate(next_json_bytes) > request.budget.max_tokens
            {
                traversal_truncated = true;
                continuable = true;
                break 'traversal;
            }
            estimated_json_bytes = next_json_bytes;
            *usage.entry(edge.from.repository()).or_default() += 1;
            *usage.entry(edge.to.repository()).or_default() += 1;
            rows.push(edge.clone());
            continuation_offset = ordinal;
        }
    }
    cancellation.check()?;
    let continuation = continuable.then_some(WorkflowContinuation {
        snapshot: snapshot.id(),
        overlay: overlay.id(),
        fingerprint,
        offset: continuation_offset,
    });
    let repository_usage = usage
        .into_iter()
        .map(|(repository, rows)| RepositoryUsage { repository, rows })
        .collect();
    Ok(WorkflowResult {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        kind: request.kind,
        snapshot: snapshot.id(),
        overlay: overlay.id(),
        edges_scanned: ordinal.min(request.budget.max_edges),
        rows,
        repository_usage,
        repository_failures: snapshot.failures().to_vec(),
        source_bytes: 0,
        estimated_json_bytes,
        estimated_tokens: token_estimate(estimated_json_bytes),
        truncated: result_incomplete || traversal_truncated,
        continuation,
    })
}

fn directed_edges(
    kind: WorkflowKind,
    overlay: &LinkOverlay,
    limit: usize,
    cancellation: &Cancellation,
) -> Result<DirectedEdges, WorkflowError> {
    let mut directed = Vec::new();
    let mut truncated = false;
    'links: for (index, link) in overlay.links().iter().enumerate() {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        for candidate in link.candidates() {
            if directed.len() >= limit {
                truncated = true;
                break 'links;
            }
            let consumer_to_provider = WorkflowEdge {
                id: edge_id(link.id(), link.consumer(), candidate.endpoint()),
                link: link.id(),
                kind: link.kind(),
                key: link.key().clone(),
                from: link.consumer(),
                to: candidate.endpoint(),
                confidence: candidate.confidence(),
                caveats: candidate.caveats().to_vec(),
            };
            match kind {
                WorkflowKind::Flow => {
                    push_directed(&mut directed, consumer_to_provider)?;
                }
                WorkflowKind::Impact => {
                    push_directed(&mut directed, reverse_edge(consumer_to_provider))?;
                }
                WorkflowKind::Context | WorkflowKind::Plan | WorkflowKind::Migration => {
                    push_directed(&mut directed, consumer_to_provider.clone())?;
                    if directed.len() >= limit {
                        truncated = true;
                        break 'links;
                    }
                    push_directed(&mut directed, reverse_edge(consumer_to_provider))?;
                }
            }
        }
    }
    directed.sort_by_key(|directed| directed.edge.id);
    Ok(DirectedEdges {
        edges: directed,
        truncated,
    })
}

fn push_directed(
    directed: &mut Vec<DirectedEdge>,
    edge: WorkflowEdge,
) -> Result<(), WorkflowError> {
    directed
        .try_reserve(1)
        .map_err(|_| WorkflowError::Accounting)?;
    directed.push(DirectedEdge { edge });
    Ok(())
}

fn reverse_edge(mut edge: WorkflowEdge) -> WorkflowEdge {
    std::mem::swap(&mut edge.from, &mut edge.to);
    edge.id = edge_id(edge.link, edge.from, edge.to);
    edge
}

fn adjacency(
    directed: &[DirectedEdge],
) -> Result<BTreeMap<WorkspaceFactRef, Vec<usize>>, WorkflowError> {
    let mut adjacency = BTreeMap::<WorkspaceFactRef, Vec<usize>>::new();
    for (index, edge) in directed.iter().enumerate() {
        let outbound = adjacency.entry(edge.edge.from).or_default();
        outbound
            .try_reserve(1)
            .map_err(|_| WorkflowError::Accounting)?;
        outbound.push(index);
    }
    Ok(adjacency)
}

fn edge_id(link: ContentHash, from: WorkspaceFactRef, to: WorkspaceFactRef) -> ContentHash {
    identity_hash(
        b"rootlight/workspace-workflow-edge/v1",
        &[
            link.as_bytes(),
            from.repository().as_bytes(),
            from.generation().as_bytes(),
            from.fact().as_bytes(),
            to.repository().as_bytes(),
            to.generation().as_bytes(),
            to.fact().as_bytes(),
        ],
    )
}

fn workflow_fingerprint(
    kind: WorkflowKind,
    seeds: &[WorkspaceFactRef],
    max_depth: usize,
) -> ContentHash {
    let mut input = Vec::with_capacity(seeds.len().saturating_mul(56).saturating_add(9));
    input.push(workflow_kind_code(kind));
    input.extend_from_slice(&u64::try_from(max_depth).unwrap_or(u64::MAX).to_be_bytes());
    for seed in seeds {
        input.extend_from_slice(seed.repository().as_bytes());
        input.extend_from_slice(seed.generation().as_bytes());
        input.extend_from_slice(seed.fact().as_bytes());
    }
    identity_hash(b"rootlight/workspace-workflow-query/v1", &[&input])
}

const fn workflow_kind_code(kind: WorkflowKind) -> u8 {
    match kind {
        WorkflowKind::Impact => 0,
        WorkflowKind::Flow => 1,
        WorkflowKind::Context => 2,
        WorkflowKind::Plan => 3,
        WorkflowKind::Migration => 4,
    }
}

const fn token_estimate(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
}

/// Invalid, inconsistent, unbounded, or cancelled workspace workflow.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowError {
    /// Budget is zero or broader than a hard process ceiling.
    #[error("workspace workflow budget is invalid")]
    InvalidBudget,
    /// Request contains no seed endpoint.
    #[error("workspace workflow has no seed")]
    EmptySeeds,
    /// Seed count exceeds the hard bound.
    #[error("workspace workflow seed limit exceeded")]
    SeedLimit,
    /// Seed does not name an exact snapshot member generation.
    #[error("workspace workflow seed is outside the snapshot")]
    SeedOutsideSnapshot,
    /// Link overlay belongs to another snapshot.
    #[error("workspace workflow overlay does not match the snapshot")]
    OverlayMismatch,
    /// Continuation belongs to another immutable query.
    #[error("workspace workflow continuation does not match")]
    ContinuationMismatch,
    /// Snapshot failure metadata alone exceeds response budgets.
    #[error("workspace workflow response metadata exceeds its budget")]
    ResponseLimit,
    /// Deterministic serialization failed.
    #[error("workspace workflow serialization failed")]
    Serialization,
    /// Bounded integer or index accounting failed.
    #[error("workspace workflow resource accounting failed")]
    Accounting,
    /// Cooperative cancellation won.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}
