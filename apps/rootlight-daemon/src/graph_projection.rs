//! Process-local bounded graph projection snapshots and sequential page cursors.
//!
//! Snapshots retain only source-free, exact-generation query results. Random
//! cursors are owner-bound by the registry and expire on monotonic deadlines.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use rootlight_operations::ClientInstanceId;
use rootlight_protocol::{
    MAX_GRAPH_PAGE_EDGES, MAX_GRAPH_PAGE_NODES, UI_GRAPH_SCHEMA_VERSION,
    generated::{daemon::v1 as daemon, ui::graph::v1 as graph},
    seal_graph_page,
};

pub(crate) const MAX_GRAPH_AGGREGATE_NODES: u32 = 512;
pub(crate) const MAX_GRAPH_AGGREGATE_EDGES: u32 = 2_048;

const MAX_ARCHITECTURE_NODES: u32 = 250;
// Architecture nodes can contribute four independent dictionary strings.
// This keeps every maximum-size page below the 512-entry wire dictionary cap.
const MAX_ARCHITECTURE_PAGE_NODES: u32 = 127;
const MAX_RELATIONSHIP_EDGES: u32 = 500;
const MAX_ACTIVE_PROJECTIONS: usize = 64;
const MAX_CLIENT_PROJECTIONS: usize = 8;
const MAX_PROJECTION_BYTES: usize = 2 * 1024 * 1024;
const MAX_REGISTRY_BYTES: usize = 16 * 1024 * 1024;
const PROJECTION_TTL: Duration = Duration::from_secs(5 * 60);
const PROJECTION_IDLE_TTL: Duration = Duration::from_secs(60);
const PROJECTION_ID_BYTES: usize = 16;
const CURSOR_BYTES: usize = 32;
const RANDOM_ATTEMPTS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EffectiveGraphBudget {
    pub(crate) page_nodes: u32,
    pub(crate) page_edges: u32,
    pub(crate) aggregate_nodes: u32,
    pub(crate) aggregate_edges: u32,
}

impl EffectiveGraphBudget {
    pub(crate) fn from_request(
        request: &daemon::GraphProjectionOpenRequest,
        view: graph::ProjectionView,
    ) -> Result<Self, GraphProjectionError> {
        let requested = request
            .budget
            .as_ref()
            .ok_or(GraphProjectionError::InvalidRequest)?;
        if requested.page_nodes == 0
            || requested.page_edges == 0
            || requested.aggregate_nodes < requested.page_nodes
            || requested.aggregate_edges < requested.page_edges
        {
            return Err(GraphProjectionError::InvalidRequest);
        }
        let view_node_limit = match view {
            graph::ProjectionView::Architecture | graph::ProjectionView::Files => {
                MAX_ARCHITECTURE_NODES
            }
            graph::ProjectionView::Symbols | graph::ProjectionView::Neighborhood => {
                MAX_GRAPH_AGGREGATE_NODES
            }
            graph::ProjectionView::Unspecified => {
                return Err(GraphProjectionError::InvalidRequest);
            }
        };
        let view_edge_limit = match view {
            graph::ProjectionView::Architecture | graph::ProjectionView::Files => {
                MAX_GRAPH_AGGREGATE_EDGES.min(1_000)
            }
            graph::ProjectionView::Symbols | graph::ProjectionView::Neighborhood => {
                MAX_RELATIONSHIP_EDGES
            }
            graph::ProjectionView::Unspecified => {
                return Err(GraphProjectionError::InvalidRequest);
            }
        };
        let aggregate_nodes = requested
            .aggregate_nodes
            .min(MAX_GRAPH_AGGREGATE_NODES)
            .min(view_node_limit);
        let aggregate_edges = requested
            .aggregate_edges
            .min(MAX_GRAPH_AGGREGATE_EDGES)
            .min(view_edge_limit);
        let page_node_limit = if matches!(
            view,
            graph::ProjectionView::Architecture | graph::ProjectionView::Files
        ) {
            MAX_ARCHITECTURE_PAGE_NODES
        } else {
            u32::try_from(MAX_GRAPH_PAGE_NODES).unwrap_or(u32::MAX)
        };
        Ok(Self {
            page_nodes: requested
                .page_nodes
                .min(page_node_limit)
                .min(aggregate_nodes),
            page_edges: requested
                .page_edges
                .min(u32::try_from(MAX_GRAPH_PAGE_EDGES).unwrap_or(u32::MAX))
                .min(aggregate_edges),
            aggregate_nodes,
            aggregate_edges,
        })
    }

    fn to_wire(self) -> daemon::GraphProjectionEffectiveBudget {
        daemon::GraphProjectionEffectiveBudget {
            page_nodes: self.page_nodes,
            page_edges: self.page_edges,
            aggregate_nodes: self.aggregate_nodes,
            aggregate_edges: self.aggregate_edges,
        }
    }
}

pub(crate) enum GraphProjectionSource {
    Architecture {
        response: daemon::ArchitectureOverviewResponse,
        view: graph::ProjectionView,
    },
    Relationships(daemon::SymbolRelationshipsResponse),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphProjectionError {
    InvalidRequest,
    InvalidCursor,
    NotFound,
    ResourceExhausted,
    RandomUnavailable,
    InvalidPage,
}

#[derive(Debug)]
pub(crate) struct GraphProjectionRegistry {
    entries: BTreeMap<[u8; PROJECTION_ID_BYTES], GraphProjection>,
    retained_bytes: usize,
}

impl GraphProjectionRegistry {
    pub(crate) fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            retained_bytes: 0,
        }
    }

    pub(crate) fn remove_repository(&mut self, repository: &[u8]) {
        let mut removed_bytes = 0_usize;
        self.entries.retain(|_, projection| {
            let matches = projection
                .context
                .repository
                .as_ref()
                .is_some_and(|candidate| candidate.value.as_slice() == repository);
            if matches {
                removed_bytes =
                    removed_bytes.saturating_add(projection.retained_bytes().unwrap_or(0));
            }
            !matches
        });
        self.retained_bytes = self.retained_bytes.saturating_sub(removed_bytes);
    }

    pub(crate) fn open(
        &mut self,
        owner: ClientInstanceId,
        request: &daemon::GraphProjectionOpenRequest,
        budget: EffectiveGraphBudget,
        source: GraphProjectionSource,
    ) -> Result<daemon::GraphProjectionResponse, GraphProjectionError> {
        let now = Instant::now();
        self.remove_expired(now);
        if self.entries.len() >= MAX_ACTIVE_PROJECTIONS
            || self
                .entries
                .values()
                .filter(|projection| projection.owner == owner)
                .count()
                >= MAX_CLIENT_PROJECTIONS
        {
            return Err(GraphProjectionError::ResourceExhausted);
        }
        let projection_id = self.random_projection_id()?;
        let mut projection =
            GraphProjection::from_source(owner, projection_id, request, budget, source, now)?;
        let retained_bytes = projection.retained_bytes()?;
        if retained_bytes > MAX_PROJECTION_BYTES
            || self
                .retained_bytes
                .checked_add(retained_bytes)
                .is_none_or(|total| total > MAX_REGISTRY_BYTES)
        {
            return Err(GraphProjectionError::ResourceExhausted);
        }
        let response = projection.next_page()?;
        self.retained_bytes = self
            .retained_bytes
            .checked_add(retained_bytes)
            .ok_or(GraphProjectionError::ResourceExhausted)?;
        self.entries.insert(projection_id, projection);
        Ok(response)
    }

    pub(crate) fn page(
        &mut self,
        owner: ClientInstanceId,
        request: &daemon::GraphProjectionPageRequest,
    ) -> Result<daemon::GraphProjectionResponse, GraphProjectionError> {
        let now = Instant::now();
        self.remove_expired(now);
        let projection_id = parse_projection_id(&request.projection_id)?;
        let projection = self
            .entries
            .get_mut(&projection_id)
            .ok_or(GraphProjectionError::NotFound)?;
        if projection.owner != owner {
            return Err(GraphProjectionError::NotFound);
        }
        let expected = projection
            .next_cursor
            .as_ref()
            .ok_or(GraphProjectionError::InvalidCursor)?;
        if !constant_time_equal(expected, &request.cursor) {
            return Err(GraphProjectionError::InvalidCursor);
        }
        projection.next_cursor = None;
        projection.last_access = now;
        projection.next_page()
    }

    pub(crate) fn release(
        &mut self,
        owner: ClientInstanceId,
        request: &daemon::GraphProjectionReleaseRequest,
    ) -> Result<daemon::GraphProjectionReleaseResponse, GraphProjectionError> {
        let now = Instant::now();
        self.remove_expired(now);
        let projection_id = parse_projection_id(&request.projection_id)?;
        if self
            .entries
            .get(&projection_id)
            .is_none_or(|projection| projection.owner != owner)
        {
            return Err(GraphProjectionError::NotFound);
        }
        let projection = self
            .entries
            .remove(&projection_id)
            .ok_or(GraphProjectionError::NotFound)?;
        self.retained_bytes = self
            .retained_bytes
            .saturating_sub(projection.retained_bytes().unwrap_or(0));
        Ok(daemon::GraphProjectionReleaseResponse {
            schema_version: Some(first_slice_schema()),
            projection_id: projection_id.to_vec(),
            released: true,
        })
    }

    fn random_projection_id(&self) -> Result<[u8; PROJECTION_ID_BYTES], GraphProjectionError> {
        for _ in 0..RANDOM_ATTEMPTS {
            let mut id = [0_u8; PROJECTION_ID_BYTES];
            getrandom::fill(&mut id).map_err(|_| GraphProjectionError::RandomUnavailable)?;
            if !self.entries.contains_key(&id) {
                return Ok(id);
            }
        }
        Err(GraphProjectionError::RandomUnavailable)
    }

    fn remove_expired(&mut self, now: Instant) {
        let mut removed_bytes = 0_usize;
        self.entries.retain(|_, projection| {
            if projection.is_expired(now) {
                removed_bytes =
                    removed_bytes.saturating_add(projection.retained_bytes().unwrap_or(0));
                false
            } else {
                true
            }
        });
        self.retained_bytes = self.retained_bytes.saturating_sub(removed_bytes);
    }
}

#[derive(Debug)]
struct GraphProjection {
    owner: ClientInstanceId,
    projection_id: [u8; PROJECTION_ID_BYTES],
    context: daemon::FirstSliceQueryContext,
    base_completeness: daemon::FirstSliceCompleteness,
    budget: EffectiveGraphBudget,
    nodes: Vec<ProjectionNode>,
    edges: Vec<ProjectionEdge>,
    node_offset: usize,
    edge_offset: usize,
    next_cursor: Option<[u8; CURSOR_BYTES]>,
    total_known_nodes: Option<u64>,
    total_known_edges: Option<u64>,
    edges_omitted_for_unavailable_endpoints: u64,
    skipped_for_coverage: u64,
    created: Instant,
    last_access: Instant,
}

impl GraphProjection {
    fn from_source(
        owner: ClientInstanceId,
        projection_id: [u8; PROJECTION_ID_BYTES],
        request: &daemon::GraphProjectionOpenRequest,
        budget: EffectiveGraphBudget,
        source: GraphProjectionSource,
        now: Instant,
    ) -> Result<Self, GraphProjectionError> {
        let (context, base_completeness, nodes, mut edges, omitted) = match source {
            GraphProjectionSource::Architecture { response, view } => {
                architecture_projection(response, view, budget)?
            }
            GraphProjectionSource::Relationships(response) => {
                relationships_projection(response, budget)?
            }
        };
        if context.repository.as_ref() != request.repository.as_ref()
            || context.generation.as_ref() != request.generation.as_ref()
        {
            return Err(GraphProjectionError::InvalidRequest);
        }
        edges.sort_by(|left, right| {
            left.source
                .max(left.target)
                .cmp(&right.source.max(right.target))
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| left.target.cmp(&right.target))
                .then_with(|| left.relation.cmp(&right.relation))
        });
        let complete = completeness_is_complete(&base_completeness);
        let total_nodes =
            u64::try_from(nodes.len()).map_err(|_| GraphProjectionError::ResourceExhausted)?;
        let total_edges =
            u64::try_from(edges.len()).map_err(|_| GraphProjectionError::ResourceExhausted)?;
        let skipped_for_coverage = context.skipped_inputs;
        Ok(Self {
            owner,
            projection_id,
            context,
            base_completeness,
            budget,
            nodes,
            edges,
            node_offset: 0,
            edge_offset: 0,
            next_cursor: None,
            total_known_nodes: complete.then_some(total_nodes),
            total_known_edges: complete.then_some(total_edges),
            edges_omitted_for_unavailable_endpoints: omitted,
            skipped_for_coverage,
            created: now,
            last_access: now,
        })
    }

    fn next_page(&mut self) -> Result<daemon::GraphProjectionResponse, GraphProjectionError> {
        let node_limit = usize::try_from(self.budget.page_nodes)
            .map_err(|_| GraphProjectionError::InvalidRequest)?;
        let edge_limit = usize::try_from(self.budget.page_edges)
            .map_err(|_| GraphProjectionError::InvalidRequest)?;
        let node_end = self
            .node_offset
            .saturating_add(node_limit)
            .min(self.nodes.len());
        let cumulative_nodes =
            u64::try_from(node_end).map_err(|_| GraphProjectionError::ResourceExhausted)?;
        let eligible_edge_end = self
            .edges
            .partition_point(|edge| {
                usize::try_from(edge.source.max(edge.target))
                    .is_ok_and(|ordinal| ordinal < node_end)
            })
            .min(self.edge_offset.saturating_add(edge_limit));
        let page_nodes = &self.nodes[self.node_offset..node_end];
        let page_edges = &self.edges[self.edge_offset..eligible_edge_end];
        self.node_offset = node_end;
        self.edge_offset = eligible_edge_end;
        let has_more = self.node_offset < self.nodes.len() || self.edge_offset < self.edges.len();
        self.next_cursor = has_more.then(random_cursor).transpose()?;
        let mut page = build_graph_page(
            page_nodes,
            page_edges,
            cumulative_nodes,
            u64::try_from(self.edge_offset).map_err(|_| GraphProjectionError::ResourceExhausted)?,
            u64::try_from(self.nodes.len()).map_err(|_| GraphProjectionError::ResourceExhausted)?,
            u64::try_from(self.edges.len()).map_err(|_| GraphProjectionError::ResourceExhausted)?,
            self.total_known_nodes,
            self.total_known_edges,
            self.edges_omitted_for_unavailable_endpoints,
            self.skipped_for_coverage,
        )?;
        seal_graph_page(&mut page).map_err(|_| GraphProjectionError::InvalidPage)?;
        Ok(daemon::GraphProjectionResponse {
            schema_version: Some(first_slice_schema()),
            projection_id: self.projection_id.to_vec(),
            next_cursor: self.next_cursor.map(|cursor| cursor.to_vec()),
            context: Some(self.context.clone()),
            completeness: Some(if has_more {
                paginated_completeness(&self.base_completeness)
            } else {
                self.base_completeness.clone()
            }),
            effective_budget: Some(self.budget.to_wire()),
            page: Some(page),
        })
    }

    fn retained_bytes(&self) -> Result<usize, GraphProjectionError> {
        let node_bytes = self.nodes.iter().try_fold(0_usize, |total, node| {
            total
                .checked_add(node.stable_id.len())
                .and_then(|total| total.checked_add(node.label.len()))
                .and_then(|total| {
                    node.path
                        .as_ref()
                        .map_or(Some(total), |path| total.checked_add(path.len()))
                })
                .and_then(|total| {
                    node.community
                        .as_ref()
                        .map_or(Some(total), |value| total.checked_add(value.len()))
                })
                .and_then(|total| {
                    node.component
                        .as_ref()
                        .map_or(Some(total), |value| total.checked_add(value.len()))
                })
                .and_then(|total| total.checked_add(size_of::<ProjectionNode>()))
                .ok_or(GraphProjectionError::ResourceExhausted)
        })?;
        node_bytes
            .checked_add(
                self.edges
                    .len()
                    .checked_mul(size_of::<ProjectionEdge>())
                    .ok_or(GraphProjectionError::ResourceExhausted)?,
            )
            .ok_or(GraphProjectionError::ResourceExhausted)
    }

    fn is_expired(&self, now: Instant) -> bool {
        now.checked_duration_since(self.created)
            .is_none_or(|age| age >= PROJECTION_TTL)
            || now
                .checked_duration_since(self.last_access)
                .is_none_or(|idle| idle >= PROJECTION_IDLE_TTL)
    }
}

#[derive(Debug)]
struct ProjectionNode {
    ordinal: u32,
    stable_id: String,
    label: String,
    path: Option<String>,
    id_kind: graph::NodeIdKind,
    kind: graph::NodeKind,
    confidence: u32,
    generated: Option<bool>,
    community: Option<String>,
    component: Option<String>,
    symbol_count: Option<u32>,
    fan_in: Option<u32>,
    fan_out: Option<u32>,
    hotspot_score: Option<u32>,
    evidence: graph::EvidenceClass,
}

#[derive(Debug)]
struct ProjectionEdge {
    source: u32,
    target: u32,
    relation: graph::RelationKind,
    weight: u32,
    confidence: u32,
    exact: bool,
    inferred: bool,
    evidence_count: u32,
}

type ProjectionParts = (
    daemon::FirstSliceQueryContext,
    daemon::FirstSliceCompleteness,
    Vec<ProjectionNode>,
    Vec<ProjectionEdge>,
    u64,
);

fn architecture_projection(
    response: daemon::ArchitectureOverviewResponse,
    _view: graph::ProjectionView,
    budget: EffectiveGraphBudget,
) -> Result<ProjectionParts, GraphProjectionError> {
    let context = response
        .context
        .ok_or(GraphProjectionError::InvalidRequest)?;
    let mut completeness = response
        .completeness
        .ok_or(GraphProjectionError::InvalidRequest)?;
    let max_nodes = usize::try_from(budget.aggregate_nodes)
        .map_err(|_| GraphProjectionError::InvalidRequest)?;
    let max_edges = usize::try_from(budget.aggregate_edges)
        .map_err(|_| GraphProjectionError::InvalidRequest)?;
    let truncated_nodes = response.components.len() > max_nodes;
    let components: Vec<_> = response.components.into_iter().take(max_nodes).collect();
    let mut ordinals = BTreeMap::new();
    for (index, component) in components.iter().enumerate() {
        let ordinal = u32::try_from(index).map_err(|_| GraphProjectionError::ResourceExhausted)?;
        ordinals.insert(component.id.clone(), ordinal);
    }
    let hotspots: BTreeMap<_, _> = response
        .hotspots
        .into_iter()
        .map(|hotspot| (hotspot.component_id.clone(), hotspot))
        .collect();
    let mut communities = BTreeMap::new();
    for community in response.communities {
        for member in community.members {
            communities.insert(member, community.id.clone());
        }
    }
    let mut nodes = Vec::new();
    nodes
        .try_reserve_exact(components.len())
        .map_err(|_| GraphProjectionError::ResourceExhausted)?;
    for component in components {
        let ordinal = *ordinals
            .get(&component.id)
            .ok_or(GraphProjectionError::InvalidRequest)?;
        let hotspot = hotspots.get(&component.id);
        nodes.push(ProjectionNode {
            ordinal,
            stable_id: component.id.clone(),
            label: component.name.clone(),
            path: Some(component.name),
            id_kind: graph::NodeIdKind::File,
            kind: graph::NodeKind::File,
            confidence: component.confidence,
            generated: None,
            community: communities.get(&component.id).cloned(),
            component: Some(component.id),
            symbol_count: Some(component.symbol_count),
            fan_in: hotspot.map(|hotspot| hotspot.fan_in),
            fan_out: hotspot.map(|hotspot| hotspot.fan_out),
            hotspot_score: hotspot.map(|hotspot| hotspot.score),
            evidence: graph::EvidenceClass::Aggregated,
        });
    }
    let mut omitted = 0_u64;
    let mut edges = Vec::new();
    for connection in response.connections {
        let (Some(source), Some(target)) = (
            ordinals.get(&connection.from).copied(),
            ordinals.get(&connection.to).copied(),
        ) else {
            omitted = omitted.saturating_add(1);
            continue;
        };
        if edges.len() >= max_edges {
            omitted = omitted.saturating_add(1);
            continue;
        }
        edges.push(ProjectionEdge {
            source,
            target,
            relation: relation_from_label(&connection.kind)
                .ok_or(GraphProjectionError::InvalidRequest)?,
            weight: connection.weight.max(1),
            confidence: connection.confidence,
            exact: false,
            inferred: false,
            evidence_count: connection.weight.max(1),
        });
    }
    if truncated_nodes || omitted > 0 {
        completeness = aggregate_truncated_completeness(completeness);
    }
    Ok((context, completeness, nodes, edges, omitted))
}

fn relationships_projection(
    response: daemon::SymbolRelationshipsResponse,
    budget: EffectiveGraphBudget,
) -> Result<ProjectionParts, GraphProjectionError> {
    let context = response
        .context
        .ok_or(GraphProjectionError::InvalidRequest)?;
    let mut completeness = response
        .completeness
        .ok_or(GraphProjectionError::InvalidRequest)?;
    let max_nodes = usize::try_from(budget.aggregate_nodes)
        .map_err(|_| GraphProjectionError::InvalidRequest)?;
    let max_edges = usize::try_from(budget.aggregate_edges)
        .map_err(|_| GraphProjectionError::InvalidRequest)?;
    let mut stable_ids = BTreeSet::new();
    for group in &response.groups {
        stable_ids.insert(symbol_label(
            group
                .seed
                .as_ref()
                .ok_or(GraphProjectionError::InvalidRequest)?,
        )?);
        for item in &group.items {
            if item.source_refs.is_empty() {
                continue;
            }
            stable_ids.insert(symbol_label(
                item.symbol
                    .as_ref()
                    .ok_or(GraphProjectionError::InvalidRequest)?,
            )?);
        }
    }
    let truncated_nodes = stable_ids.len() > max_nodes;
    let stable_ids: Vec<_> = stable_ids.into_iter().take(max_nodes).collect();
    let mut ordinals = BTreeMap::new();
    let mut nodes = Vec::new();
    for (index, stable_id) in stable_ids.into_iter().enumerate() {
        let ordinal = u32::try_from(index).map_err(|_| GraphProjectionError::ResourceExhausted)?;
        ordinals.insert(stable_id.clone(), ordinal);
        nodes.push(ProjectionNode {
            ordinal,
            label: stable_id.clone(),
            stable_id,
            path: None,
            id_kind: graph::NodeIdKind::Symbol,
            kind: graph::NodeKind::Symbol,
            confidence: 1_000,
            generated: None,
            community: None,
            component: None,
            symbol_count: None,
            fan_in: None,
            fan_out: None,
            hotspot_score: None,
            evidence: graph::EvidenceClass::Candidate,
        });
    }
    let mut omitted = 0_u64;
    let mut edges = Vec::new();
    for group in response.groups {
        let seed = symbol_label(
            group
                .seed
                .as_ref()
                .ok_or(GraphProjectionError::InvalidRequest)?,
        )?;
        let relation =
            relation_from_label(&group.relation).ok_or(GraphProjectionError::InvalidRequest)?;
        for item in group.items {
            // The request contract excludes inferred relations. An evidence-free
            // candidate is filtered before snapshot totals are established.
            if item.source_refs.is_empty() {
                continue;
            }
            let target = symbol_label(
                item.symbol
                    .as_ref()
                    .ok_or(GraphProjectionError::InvalidRequest)?,
            )?;
            let (Some(seed_ordinal), Some(target_ordinal)) =
                (ordinals.get(&seed).copied(), ordinals.get(&target).copied())
            else {
                omitted = omitted.saturating_add(1);
                continue;
            };
            if edges.len() >= max_edges {
                omitted = omitted.saturating_add(1);
                continue;
            }
            let (source, target) = if group.direction == "inbound" {
                (target_ordinal, seed_ordinal)
            } else {
                (seed_ordinal, target_ordinal)
            };
            let evidence_count = u32::try_from(item.source_refs.len()).unwrap_or(u32::MAX);
            edges.push(ProjectionEdge {
                source,
                target,
                relation,
                weight: 1,
                confidence: item.confidence,
                exact: true,
                inferred: false,
                evidence_count,
            });
        }
    }
    if truncated_nodes || response.truncated || omitted > 0 {
        completeness = aggregate_truncated_completeness(completeness);
    }
    Ok((context, completeness, nodes, edges, omitted))
}

#[expect(
    clippy::too_many_arguments,
    reason = "page counters are independent wire contract dimensions"
)]
fn build_graph_page(
    nodes: &[ProjectionNode],
    edges: &[ProjectionEdge],
    returned_nodes_cumulative: u64,
    returned_edges_cumulative: u64,
    total_matching_nodes: u64,
    total_matching_edges: u64,
    total_known_nodes: Option<u64>,
    total_known_edges: Option<u64>,
    edges_omitted_for_unavailable_endpoints: u64,
    skipped_for_coverage: u64,
) -> Result<graph::GraphPage, GraphProjectionError> {
    let mut strings = vec![String::new()];
    let mut dictionary = BTreeMap::new();
    let mut wire_nodes = Vec::new();
    wire_nodes
        .try_reserve_exact(nodes.len())
        .map_err(|_| GraphProjectionError::ResourceExhausted)?;
    for node in nodes {
        wire_nodes.push(graph::GraphNode {
            ordinal: node.ordinal,
            stable_id: node.stable_id.clone(),
            id_kind: node.id_kind as i32,
            label_index: intern_string(&mut strings, &mut dictionary, &node.label)?,
            path_index: node
                .path
                .as_deref()
                .map(|value| intern_string(&mut strings, &mut dictionary, value))
                .transpose()?,
            kind: node.kind as i32,
            confidence: node.confidence,
            generated: node.generated,
            community_index: node
                .community
                .as_deref()
                .map(|value| intern_string(&mut strings, &mut dictionary, value))
                .transpose()?,
            component_index: node
                .component
                .as_deref()
                .map(|value| intern_string(&mut strings, &mut dictionary, value))
                .transpose()?,
            symbol_count: node.symbol_count,
            fan_in: node.fan_in,
            fan_out: node.fan_out,
            hotspot_score: node.hotspot_score,
            evidence: node.evidence as i32,
        });
    }
    let wire_edges = edges
        .iter()
        .map(|edge| graph::GraphEdge {
            source_ordinal: edge.source,
            target_ordinal: edge.target,
            relation: edge.relation as i32,
            weight: edge.weight,
            confidence: edge.confidence,
            exact: edge.exact,
            inferred: edge.inferred,
            evidence_count: edge.evidence_count,
            overlay: graph::OverlayRole::None as i32,
        })
        .collect();
    Ok(graph::GraphPage {
        schema_version: UI_GRAPH_SCHEMA_VERSION,
        strings,
        nodes: wire_nodes,
        edges: wire_edges,
        returned_nodes_cumulative,
        returned_edges_cumulative,
        total_matching_nodes: Some(total_matching_nodes),
        total_matching_edges: Some(total_matching_edges),
        total_known_nodes,
        total_known_edges,
        edges_omitted_for_unavailable_endpoints,
        skipped_for_coverage,
        checksum: Vec::new(),
    })
}

fn intern_string(
    strings: &mut Vec<String>,
    dictionary: &mut BTreeMap<String, u32>,
    value: &str,
) -> Result<u32, GraphProjectionError> {
    if let Some(index) = dictionary.get(value) {
        return Ok(*index);
    }
    let index =
        u32::try_from(strings.len()).map_err(|_| GraphProjectionError::ResourceExhausted)?;
    strings
        .try_reserve(1)
        .map_err(|_| GraphProjectionError::ResourceExhausted)?;
    strings.push(value.to_owned());
    dictionary.insert(value.to_owned(), index);
    Ok(index)
}

pub(crate) fn relation_label(relation: graph::RelationKind) -> Option<&'static str> {
    match relation {
        graph::RelationKind::Calls => Some("calls"),
        graph::RelationKind::CalledBy => Some("called_by"),
        graph::RelationKind::References => Some("references"),
        graph::RelationKind::Types => Some("types"),
        graph::RelationKind::Implements => Some("implements"),
        graph::RelationKind::Imports => Some("imports"),
        graph::RelationKind::Tests => Some("tests"),
        graph::RelationKind::Ownership => Some("ownership"),
        graph::RelationKind::ServiceCall => Some("service_call"),
        graph::RelationKind::CallsRoute => Some("calls_route"),
        graph::RelationKind::Messaging => Some("messaging"),
        graph::RelationKind::ReadsTable => Some("reads_table"),
        graph::RelationKind::WritesTable => Some("writes_table"),
        graph::RelationKind::BuildDependency => Some("build_dependency"),
        graph::RelationKind::DataFlow => Some("data_flow"),
        graph::RelationKind::History => Some("history"),
        graph::RelationKind::Unspecified => None,
    }
}

fn relation_from_label(label: &str) -> Option<graph::RelationKind> {
    [
        graph::RelationKind::Calls,
        graph::RelationKind::CalledBy,
        graph::RelationKind::References,
        graph::RelationKind::Types,
        graph::RelationKind::Implements,
        graph::RelationKind::Imports,
        graph::RelationKind::Tests,
        graph::RelationKind::Ownership,
        graph::RelationKind::ServiceCall,
        graph::RelationKind::CallsRoute,
        graph::RelationKind::Messaging,
        graph::RelationKind::ReadsTable,
        graph::RelationKind::WritesTable,
        graph::RelationKind::BuildDependency,
        graph::RelationKind::DataFlow,
        graph::RelationKind::History,
    ]
    .into_iter()
    .find(|relation| relation_label(*relation) == Some(label))
}

fn paginated_completeness(base: &daemon::FirstSliceCompleteness) -> daemon::FirstSliceCompleteness {
    let mut completeness = base.clone();
    completeness.state =
        daemon::FirstSliceCompletenessState::FirstSliceCompletenessTruncated as i32;
    if !completeness.limiting_resources.iter().any(|resource| {
        resource.kind == daemon::FirstSliceLimitingResourceKind::FirstSliceLimitPageSize as i32
    }) {
        completeness
            .limiting_resources
            .push(daemon::FirstSliceLimitingResource {
                kind: daemon::FirstSliceLimitingResourceKind::FirstSliceLimitPageSize as i32,
                limit: None,
                observed: None,
            });
    }
    completeness.continuation =
        daemon::FirstSliceContinuationAvailability::FirstSliceContinuationAvailable as i32;
    if !completeness
        .guidance
        .contains(&(daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceUseCursor as i32))
    {
        completeness
            .guidance
            .push(daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceUseCursor as i32);
    }
    completeness
}

fn aggregate_truncated_completeness(
    mut completeness: daemon::FirstSliceCompleteness,
) -> daemon::FirstSliceCompleteness {
    completeness.state =
        daemon::FirstSliceCompletenessState::FirstSliceCompletenessTruncated as i32;
    completeness.continuation =
        daemon::FirstSliceContinuationAvailability::FirstSliceContinuationUnavailable as i32;
    if !completeness.limiting_resources.iter().any(|resource| {
        resource.kind == daemon::FirstSliceLimitingResourceKind::FirstSliceLimitResults as i32
    }) {
        completeness
            .limiting_resources
            .push(daemon::FirstSliceLimitingResource {
                kind: daemon::FirstSliceLimitingResourceKind::FirstSliceLimitResults as i32,
                limit: None,
                observed: None,
            });
    }
    if !completeness
        .guidance
        .contains(&(daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceNarrowScope as i32))
    {
        completeness
            .guidance
            .push(daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceNarrowScope as i32);
    }
    completeness
}

fn completeness_is_complete(completeness: &daemon::FirstSliceCompleteness) -> bool {
    daemon::FirstSliceCompletenessState::try_from(completeness.state)
        == Ok(daemon::FirstSliceCompletenessState::FirstSliceCompletenessComplete)
}

fn random_cursor() -> Result<[u8; CURSOR_BYTES], GraphProjectionError> {
    let mut cursor = [0_u8; CURSOR_BYTES];
    getrandom::fill(&mut cursor).map_err(|_| GraphProjectionError::RandomUnavailable)?;
    Ok(cursor)
}

fn parse_projection_id(value: &[u8]) -> Result<[u8; PROJECTION_ID_BYTES], GraphProjectionError> {
    <[u8; PROJECTION_ID_BYTES]>::try_from(value).map_err(|_| GraphProjectionError::InvalidRequest)
}

fn constant_time_equal(expected: &[u8], observed: &[u8]) -> bool {
    expected.len() == observed.len()
        && expected
            .iter()
            .zip(observed)
            .fold(0_u8, |difference, (left, right)| {
                difference | (*left ^ *right)
            })
            == 0
}

fn symbol_label(
    symbol: &rootlight_protocol::generated::common::v1::SymbolId,
) -> Result<String, GraphProjectionError> {
    if symbol.value.len() != 20 {
        return Err(GraphProjectionError::InvalidRequest);
    }
    let mut label = String::new();
    label
        .try_reserve_exact(symbol.value.len().saturating_mul(2))
        .map_err(|_| GraphProjectionError::ResourceExhausted)?;
    for byte in &symbol.value {
        use std::fmt::Write as _;

        write!(&mut label, "{byte:02x}").map_err(|_| GraphProjectionError::ResourceExhausted)?;
    }
    Ok(label)
}

fn first_slice_schema() -> rootlight_protocol::generated::common::v1::ContractVersion {
    rootlight_protocol::generated::common::v1::ContractVersion { major: 1, minor: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_protocol::validate_graph_page;

    fn client(value: u8) -> ClientInstanceId {
        ClientInstanceId::from_bytes([value; 16])
    }

    fn context() -> daemon::FirstSliceQueryContext {
        daemon::FirstSliceQueryContext {
            repository: Some(rootlight_protocol::generated::common::v1::RepositoryId {
                value: vec![1; 16],
            }),
            generation: Some(rootlight_protocol::generated::common::v1::GenerationId {
                value: vec![2; 20],
            }),
            parent_generation: None,
            active_generation: true,
            tier: daemon::FirstSliceAnalysisTier::FirstSliceTierA as i32,
            coverage_status: daemon::FirstSliceCoverageStatus::FirstSliceCoverageComplete as i32,
            skipped_inputs: 0,
            usage: Some(daemon::FirstSliceQueryUsage::default()),
            structural_freshness: "current".to_owned(),
            semantic_freshness: "current".to_owned(),
        }
    }

    fn complete() -> daemon::FirstSliceCompleteness {
        daemon::FirstSliceCompleteness {
            state: daemon::FirstSliceCompletenessState::FirstSliceCompletenessComplete as i32,
            limiting_resources: Vec::new(),
            continuation:
                daemon::FirstSliceContinuationAvailability::FirstSliceContinuationNotApplicable
                    as i32,
            guidance: Vec::new(),
        }
    }

    fn request(page_nodes: u32) -> daemon::GraphProjectionOpenRequest {
        daemon::GraphProjectionOpenRequest {
            schema_version: Some(first_slice_schema()),
            repository: context().repository,
            generation: context().generation,
            view: graph::ProjectionView::Architecture as i32,
            scope: Some(daemon::GraphProjectionScope {
                scope: Some(daemon::graph_projection_scope::Scope::WholeRepository(
                    daemon::GraphProjectionWholeRepository {},
                )),
            }),
            filters: Some(daemon::GraphProjectionFilters {
                node_kinds: Vec::new(),
                relations: Vec::new(),
                languages: Vec::new(),
                min_confidence: 0,
                include_inferred: Some(false),
                include_generated: Some(true),
                community_id: None,
                hotspot_threshold: None,
            }),
            budget: Some(daemon::GraphProjectionBudget {
                page_nodes,
                page_edges: 10,
                aggregate_nodes: 10,
                aggregate_edges: 10,
            }),
        }
    }

    fn source() -> GraphProjectionSource {
        GraphProjectionSource::Architecture {
            response: daemon::ArchitectureOverviewResponse {
                schema_version: Some(first_slice_schema()),
                context: Some(context()),
                components: (0..3)
                    .map(|index| daemon::FirstSliceArchitectureComponent {
                        id: format!("file:{index}"),
                        kind: "file".to_owned(),
                        name: format!("src/{index}.rs"),
                        symbol_count: 1,
                        responsibility_evidence: vec!["contains_symbols".to_owned()],
                        confidence: 1_000,
                    })
                    .collect(),
                connections: vec![daemon::FirstSliceArchitectureConnection {
                    from: "file:0".to_owned(),
                    to: "file:2".to_owned(),
                    kind: "calls".to_owned(),
                    weight: 1,
                    confidence: 1_000,
                }],
                hotspots: Vec::new(),
                views: Vec::new(),
                completeness: Some(complete()),
                communities: Vec::new(),
            },
            view: graph::ProjectionView::Architecture,
        }
    }

    #[test]
    fn pages_only_emit_edges_after_both_endpoints_are_known() {
        let request = request(1);
        let budget =
            EffectiveGraphBudget::from_request(&request, graph::ProjectionView::Architecture)
                .expect("budget validates");
        let mut registry = GraphProjectionRegistry::new();
        let first = registry
            .open(client(1), &request, budget, source())
            .expect("projection opens");
        let first_page = first.page.as_ref().expect("page is present");
        validate_graph_page(first_page).expect("first page validates");
        assert!(first_page.edges.is_empty());

        let second = registry
            .page(
                client(1),
                &daemon::GraphProjectionPageRequest {
                    schema_version: Some(first_slice_schema()),
                    projection_id: first.projection_id.clone(),
                    cursor: first.next_cursor.expect("continuation is present"),
                },
            )
            .expect("second page loads");
        assert!(
            second
                .page
                .as_ref()
                .expect("page is present")
                .edges
                .is_empty()
        );
        let third = registry
            .page(
                client(1),
                &daemon::GraphProjectionPageRequest {
                    schema_version: Some(first_slice_schema()),
                    projection_id: second.projection_id.clone(),
                    cursor: second.next_cursor.expect("continuation is present"),
                },
            )
            .expect("third page loads");
        assert_eq!(third.page.as_ref().expect("page is present").edges.len(), 1);
    }

    #[test]
    fn cursor_is_owner_bound_single_use_and_release_frees_capacity() {
        let request = request(1);
        let budget =
            EffectiveGraphBudget::from_request(&request, graph::ProjectionView::Architecture)
                .expect("budget validates");
        let mut registry = GraphProjectionRegistry::new();
        let first = registry
            .open(client(1), &request, budget, source())
            .expect("projection opens");
        let cursor = first.next_cursor.clone().expect("continuation is present");
        let page = daemon::GraphProjectionPageRequest {
            schema_version: Some(first_slice_schema()),
            projection_id: first.projection_id.clone(),
            cursor: cursor.clone(),
        };
        assert_eq!(
            registry.page(client(2), &page),
            Err(GraphProjectionError::NotFound)
        );
        registry.page(client(1), &page).expect("owner continues");
        assert_eq!(
            registry.page(client(1), &page),
            Err(GraphProjectionError::InvalidCursor)
        );
        let released = registry
            .release(
                client(1),
                &daemon::GraphProjectionReleaseRequest {
                    schema_version: Some(first_slice_schema()),
                    projection_id: first.projection_id,
                },
            )
            .expect("owner releases");
        assert!(released.released);
        assert_eq!(registry.retained_bytes, 0);
    }

    #[test]
    fn relationship_projection_excludes_evidence_free_inference() {
        let response = daemon::SymbolRelationshipsResponse {
            schema_version: Some(first_slice_schema()),
            context: Some(context()),
            groups: vec![daemon::FirstSliceRelationshipGroup {
                seed: Some(rootlight_protocol::generated::common::v1::SymbolId {
                    value: vec![3; 20],
                }),
                relation: "calls".to_owned(),
                direction: "outbound".to_owned(),
                items: vec![daemon::FirstSliceRelationshipTarget {
                    symbol: Some(rootlight_protocol::generated::common::v1::SymbolId {
                        value: vec![4; 20],
                    }),
                    confidence: 500,
                    source_refs: Vec::new(),
                }],
                total_count: 1,
            }],
            returned_edges: 1,
            total_edges: 1,
            exact: false,
            truncated: false,
            next_page_offset: None,
            completeness: Some(complete()),
        };
        let (_, _, nodes, edges, omitted) = relationships_projection(
            response,
            EffectiveGraphBudget {
                page_nodes: 10,
                page_edges: 10,
                aggregate_nodes: 10,
                aggregate_edges: 10,
            },
        )
        .expect("bounded relationship projection builds");

        assert_eq!(nodes.len(), 1);
        assert!(edges.is_empty());
        assert_eq!(omitted, 0);
    }
}
