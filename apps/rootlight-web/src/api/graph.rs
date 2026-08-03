//! Session-owned HTTP projection over exact-generation daemon graph pages.

use std::time::{Duration, Instant};

use axum::{
    Extension, Json,
    extract::{Path, State},
};
use rootlight_client::{
    AnalysisTier, ContinuationAvailability, ContinuationGuidance, CoverageStatus, GenerationId,
    GraphEdge, GraphEvidenceClass, GraphNode, GraphNodeIdKind, GraphNodeKind, GraphOverlayRole,
    GraphProjectionBudget, GraphProjectionPage, GraphProjectionRequest, GraphProjectionView,
    GraphRelationKind, LimitingResourceKind, QueryFreshness, RepositoryId, RequestTimeout,
    ResultCompletenessState, SymbolId,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::{ApiError, AppState},
    graph_registry::{GraphRegistryError, IssuedGraphProjection},
    session::AuthenticatedSession,
};

const GRAPH_OPEN_TIMEOUT: Duration = Duration::from_secs(8);
const GRAPH_PAGE_TIMEOUT: Duration = Duration::from_secs(5);
const GRAPH_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SYMBOL_SEEDS: usize = 64;
const MAX_RELATIONS: usize = 16;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OpenGraphRequest {
    repository_id: String,
    generation_id: String,
    view: BrowserGraphView,
    #[serde(default)]
    symbol_ids: Vec<String>,
    #[serde(default)]
    relations: Vec<BrowserRelation>,
    #[serde(default)]
    min_confidence: u32,
    #[serde(default)]
    budget_profile: BrowserBudgetProfile,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BrowserGraphView {
    Architecture,
    Files,
    Symbols,
    Neighborhood,
}

impl BrowserGraphView {
    const fn client(self) -> GraphProjectionView {
        match self {
            Self::Architecture => GraphProjectionView::Architecture,
            Self::Files => GraphProjectionView::Files,
            Self::Symbols => GraphProjectionView::Symbols,
            Self::Neighborhood => GraphProjectionView::Neighborhood,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BrowserBudgetProfile {
    Compact,
    #[default]
    Balanced,
    Expanded,
}

impl BrowserBudgetProfile {
    fn budget(self) -> Result<GraphProjectionBudget, ApiError> {
        let values = match self {
            Self::Compact => (80, 180, 160, 500),
            Self::Balanced => (127, 300, 250, 750),
            Self::Expanded => (127, 500, 250, 1_000),
        };
        GraphProjectionBudget::new(values.0, values.1, values.2, values.3)
            .map_err(|_| ApiError::invalid_graph_request())
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BrowserRelation {
    Calls,
    CalledBy,
    References,
    Types,
    Implements,
    Imports,
    Tests,
    Ownership,
    ServiceCall,
    CallsRoute,
    Messaging,
    ReadsTable,
    WritesTable,
    BuildDependency,
    DataFlow,
    History,
}

impl BrowserRelation {
    const fn client(self) -> GraphRelationKind {
        match self {
            Self::Calls => GraphRelationKind::Calls,
            Self::CalledBy => GraphRelationKind::CalledBy,
            Self::References => GraphRelationKind::References,
            Self::Types => GraphRelationKind::Types,
            Self::Implements => GraphRelationKind::Implements,
            Self::Imports => GraphRelationKind::Imports,
            Self::Tests => GraphRelationKind::Tests,
            Self::Ownership => GraphRelationKind::Ownership,
            Self::ServiceCall => GraphRelationKind::ServiceCall,
            Self::CallsRoute => GraphRelationKind::CallsRoute,
            Self::Messaging => GraphRelationKind::Messaging,
            Self::ReadsTable => GraphRelationKind::ReadsTable,
            Self::WritesTable => GraphRelationKind::WritesTable,
            Self::BuildDependency => GraphRelationKind::BuildDependency,
            Self::DataFlow => GraphRelationKind::DataFlow,
            Self::History => GraphRelationKind::History,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserGraphPage {
    schema: &'static str,
    projection_token: String,
    page_ordinal: u32,
    context: BrowserGraphContext,
    nodes: Vec<BrowserGraphNode>,
    edges: Vec<BrowserGraphEdge>,
    completeness: BrowserCompleteness,
    effective_budget: BrowserGraphBudget,
    returned_nodes_cumulative: String,
    returned_edges_cumulative: String,
    total_matching_nodes: String,
    total_matching_edges: String,
    total_known_nodes: Option<String>,
    total_known_edges: Option<String>,
    edges_omitted_for_unavailable_endpoints: String,
    skipped_for_coverage: String,
    has_next_page: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserGraphContext {
    repository_id: String,
    generation_id: String,
    parent_generation_id: Option<String>,
    active_generation: bool,
    structural_freshness: &'static str,
    semantic_freshness: &'static str,
    tier: &'static str,
    coverage_status: &'static str,
    skipped_inputs: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserGraphNode {
    ordinal: u32,
    stable_id: String,
    id_kind: &'static str,
    label: String,
    path: Option<String>,
    kind: &'static str,
    confidence: u32,
    generated: Option<bool>,
    community: Option<String>,
    component: Option<String>,
    symbol_count: Option<u32>,
    fan_in: Option<u32>,
    fan_out: Option<u32>,
    hotspot_score: Option<u32>,
    evidence: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserGraphEdge {
    source_ordinal: u32,
    target_ordinal: u32,
    relation: &'static str,
    weight: u32,
    confidence: u32,
    exact: bool,
    inferred: bool,
    evidence_count: u32,
    overlay: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserCompleteness {
    state: &'static str,
    limiting_resources: Vec<BrowserLimitingResource>,
    continuation: &'static str,
    guidance: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserLimitingResource {
    kind: &'static str,
    limit: Option<String>,
    observed: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserGraphBudget {
    page_nodes: u32,
    page_edges: u32,
    aggregate_nodes: u32,
    aggregate_edges: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserGraphRelease {
    schema: &'static str,
    released: bool,
}

pub(crate) async fn open(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(body): Json<OpenGraphRequest>,
) -> Result<Json<BrowserGraphPage>, ApiError> {
    let request = checked_request(body)?;
    let page = state
        .daemon()
        .graph_projection_open(
            &request,
            RequestTimeout::try_from(GRAPH_OPEN_TIMEOUT)
                .map_err(|_| ApiError::invalid_graph_request())?,
        )
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    let issued = match state
        .graphs()
        .issue(session.identity(), &page, Instant::now())
    {
        Ok(issued) => issued,
        Err(error) => {
            let _ = state
                .daemon()
                .graph_projection_release(
                    page.projection,
                    RequestTimeout::try_from(GRAPH_RELEASE_TIMEOUT)
                        .map_err(|_| ApiError::invalid_graph_request())?,
                )
                .await;
            return Err(graph_registry_error(error));
        }
    };
    Ok(Json(browser_page(page, issued, 0)))
}

pub(crate) async fn next(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(projection_token): Path<String>,
) -> Result<Json<BrowserGraphPage>, ApiError> {
    let handle = state
        .graphs()
        .claim(session.identity(), &projection_token, Instant::now())
        .map_err(graph_registry_error)?;
    let (continuation, page_ordinal) = handle.begin_next().await.map_err(graph_registry_error)?;
    let page = match state
        .daemon()
        .graph_projection_page(
            &continuation,
            RequestTimeout::try_from(GRAPH_PAGE_TIMEOUT)
                .map_err(|_| ApiError::invalid_graph_request())?,
        )
        .await
    {
        Ok(page) => page,
        Err(error) => {
            handle.abandon_next().await;
            release_failed_projection(&state, session.identity(), &projection_token).await;
            return Err(ApiError::from_daemon(&error));
        }
    };
    if let Err(error) = handle.finish_next(&page).await {
        release_failed_projection(&state, session.identity(), &projection_token).await;
        return Err(graph_registry_error(error));
    }
    Ok(Json(browser_page(
        page,
        IssuedGraphProjection {
            token: projection_token,
        },
        page_ordinal,
    )))
}

pub(crate) async fn release(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(projection_token): Path<String>,
) -> Result<Json<BrowserGraphRelease>, ApiError> {
    let handle = state
        .graphs()
        .release(session.identity(), &projection_token)
        .map_err(graph_registry_error)?;
    let released = state
        .daemon()
        .graph_projection_release(
            handle.projection(),
            RequestTimeout::try_from(GRAPH_RELEASE_TIMEOUT)
                .map_err(|_| ApiError::invalid_graph_request())?,
        )
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    Ok(Json(BrowserGraphRelease {
        schema: "rootlight.web-graph-release/1",
        released,
    }))
}

fn checked_request(body: OpenGraphRequest) -> Result<GraphProjectionRequest, ApiError> {
    if body.min_confidence > 1_000
        || body.symbol_ids.len() > MAX_SYMBOL_SEEDS
        || body.relations.len() > MAX_RELATIONS
    {
        return Err(ApiError::invalid_graph_request());
    }
    let repository = body
        .repository_id
        .parse::<RepositoryId>()
        .map_err(|_| ApiError::invalid_graph_request())?;
    let generation = body
        .generation_id
        .parse::<GenerationId>()
        .map_err(|_| ApiError::invalid_graph_request())?;
    let view = body.view.client();
    let budget = body.budget_profile.budget()?;
    let request = match body.view {
        BrowserGraphView::Architecture | BrowserGraphView::Files => {
            if !body.symbol_ids.is_empty() || !body.relations.is_empty() {
                return Err(ApiError::invalid_graph_request());
            }
            GraphProjectionRequest::whole_repository(repository, generation, view, budget)
        }
        BrowserGraphView::Symbols | BrowserGraphView::Neighborhood => {
            let symbols = body
                .symbol_ids
                .iter()
                .map(|value| {
                    value
                        .parse::<SymbolId>()
                        .map_err(|_| ApiError::invalid_graph_request())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let relations = body
                .relations
                .into_iter()
                .map(BrowserRelation::client)
                .collect::<Vec<_>>();
            GraphProjectionRequest::symbols(
                repository, generation, view, &symbols, &relations, budget,
            )
        }
    }
    .map_err(|_| ApiError::invalid_graph_request())?;
    request
        .with_min_confidence(body.min_confidence)
        .map_err(|_| ApiError::invalid_graph_request())
}

async fn release_failed_projection(
    state: &AppState,
    owner: crate::session::SessionIdentity,
    token: &str,
) {
    let Ok(handle) = state.graphs().release(owner, token) else {
        return;
    };
    let Ok(timeout) = RequestTimeout::try_from(GRAPH_RELEASE_TIMEOUT) else {
        return;
    };
    let _ = state
        .daemon()
        .graph_projection_release(handle.projection(), timeout)
        .await;
}

fn browser_page(
    page: GraphProjectionPage,
    issued: IssuedGraphProjection,
    page_ordinal: u32,
) -> BrowserGraphPage {
    let context = BrowserGraphContext {
        repository_id: page.context.repository.to_string(),
        generation_id: page.context.generation.to_string(),
        parent_generation_id: page
            .context
            .parent_generation
            .map(|generation| generation.to_string()),
        active_generation: page.context.active_generation,
        structural_freshness: freshness(page.context.structural_freshness),
        semantic_freshness: freshness(page.context.semantic_freshness),
        tier: analysis_tier(page.context.tier),
        coverage_status: coverage_status(page.context.coverage_status),
        skipped_inputs: page.context.skipped_inputs.to_string(),
    };
    let completeness = BrowserCompleteness {
        state: completeness_state(page.completeness.state),
        limiting_resources: page
            .completeness
            .limiting_resources
            .iter()
            .map(|resource| BrowserLimitingResource {
                kind: limiting_resource(resource.kind),
                limit: resource.limit.map(|value| value.to_string()),
                observed: resource.observed.map(|value| value.to_string()),
            })
            .collect(),
        continuation: continuation_availability(page.completeness.continuation),
        guidance: page
            .completeness
            .guidance
            .iter()
            .copied()
            .map(continuation_guidance)
            .collect(),
    };
    BrowserGraphPage {
        schema: "rootlight.web-graph-page/1",
        projection_token: issued.token,
        page_ordinal,
        context,
        nodes: page.nodes.into_iter().map(browser_node).collect(),
        edges: page.edges.into_iter().map(browser_edge).collect(),
        completeness,
        effective_budget: BrowserGraphBudget {
            page_nodes: page.effective_budget.page_nodes,
            page_edges: page.effective_budget.page_edges,
            aggregate_nodes: page.effective_budget.aggregate_nodes,
            aggregate_edges: page.effective_budget.aggregate_edges,
        },
        returned_nodes_cumulative: page.returned_nodes_cumulative.to_string(),
        returned_edges_cumulative: page.returned_edges_cumulative.to_string(),
        total_matching_nodes: page.total_matching_nodes.to_string(),
        total_matching_edges: page.total_matching_edges.to_string(),
        total_known_nodes: page.total_known_nodes.map(|value| value.to_string()),
        total_known_edges: page.total_known_edges.map(|value| value.to_string()),
        edges_omitted_for_unavailable_endpoints: page
            .edges_omitted_for_unavailable_endpoints
            .to_string(),
        skipped_for_coverage: page.skipped_for_coverage.to_string(),
        has_next_page: page.continuation.is_some(),
    }
}

fn browser_node(node: GraphNode) -> BrowserGraphNode {
    BrowserGraphNode {
        ordinal: node.ordinal,
        stable_id: node.stable_id,
        id_kind: node_id_kind(node.id_kind),
        label: node.label,
        path: node.path,
        kind: node_kind(node.kind),
        confidence: node.confidence,
        generated: node.generated,
        community: node.community,
        component: node.component,
        symbol_count: node.symbol_count,
        fan_in: node.fan_in,
        fan_out: node.fan_out,
        hotspot_score: node.hotspot_score,
        evidence: evidence_class(node.evidence),
    }
}

fn browser_edge(edge: GraphEdge) -> BrowserGraphEdge {
    BrowserGraphEdge {
        source_ordinal: edge.source_ordinal,
        target_ordinal: edge.target_ordinal,
        relation: relation_kind(edge.relation),
        weight: edge.weight,
        confidence: edge.confidence,
        exact: edge.exact,
        inferred: edge.inferred,
        evidence_count: edge.evidence_count,
        overlay: overlay_role(edge.overlay),
    }
}

const fn freshness(value: QueryFreshness) -> &'static str {
    match value {
        QueryFreshness::Current => "current",
        QueryFreshness::Stale => "stale",
        QueryFreshness::Superseded => "superseded",
    }
}

const fn analysis_tier(value: AnalysisTier) -> &'static str {
    match value {
        AnalysisTier::TierA => "tier_a",
        AnalysisTier::TierB => "tier_b",
        AnalysisTier::TierC => "tier_c",
        AnalysisTier::TierD => "tier_d",
    }
}

const fn coverage_status(value: CoverageStatus) -> &'static str {
    match value {
        CoverageStatus::Complete => "complete",
        CoverageStatus::Bounded => "bounded",
        CoverageStatus::Sampled => "sampled",
        CoverageStatus::Unknown => "unknown",
    }
}

const fn node_id_kind(value: GraphNodeIdKind) -> &'static str {
    match value {
        GraphNodeIdKind::File => "file",
        GraphNodeIdKind::Symbol => "symbol",
        GraphNodeIdKind::Unknown(_) => "unknown",
    }
}

const fn node_kind(value: GraphNodeKind) -> &'static str {
    match value {
        GraphNodeKind::File => "file",
        GraphNodeKind::Symbol => "symbol",
        GraphNodeKind::Unknown(_) => "unknown",
    }
}

const fn evidence_class(value: GraphEvidenceClass) -> &'static str {
    match value {
        GraphEvidenceClass::Structural => "structural",
        GraphEvidenceClass::Aggregated => "aggregated",
        GraphEvidenceClass::Candidate => "candidate",
        GraphEvidenceClass::Unknown(_) => "unknown",
    }
}

const fn overlay_role(value: GraphOverlayRole) -> &'static str {
    match value {
        GraphOverlayRole::None => "none",
        GraphOverlayRole::Unknown(_) => "unknown",
    }
}

const fn relation_kind(value: GraphRelationKind) -> &'static str {
    match value {
        GraphRelationKind::Calls => "calls",
        GraphRelationKind::CalledBy => "called_by",
        GraphRelationKind::References => "references",
        GraphRelationKind::Types => "types",
        GraphRelationKind::Implements => "implements",
        GraphRelationKind::Imports => "imports",
        GraphRelationKind::Tests => "tests",
        GraphRelationKind::Ownership => "ownership",
        GraphRelationKind::ServiceCall => "service_call",
        GraphRelationKind::CallsRoute => "calls_route",
        GraphRelationKind::Messaging => "messaging",
        GraphRelationKind::ReadsTable => "reads_table",
        GraphRelationKind::WritesTable => "writes_table",
        GraphRelationKind::BuildDependency => "build_dependency",
        GraphRelationKind::DataFlow => "data_flow",
        GraphRelationKind::History => "history",
        GraphRelationKind::Unknown(_) => "unknown",
    }
}

const fn completeness_state(value: ResultCompletenessState) -> &'static str {
    match value {
        ResultCompletenessState::Complete => "complete",
        ResultCompletenessState::Truncated => "truncated",
        ResultCompletenessState::UnsupportedPartial => "unsupported_partial",
        ResultCompletenessState::Indeterminate => "indeterminate",
    }
}

const fn continuation_availability(value: ContinuationAvailability) -> &'static str {
    match value {
        ContinuationAvailability::NotApplicable => "not_applicable",
        ContinuationAvailability::Available => "available",
        ContinuationAvailability::Unavailable => "unavailable",
    }
}

const fn continuation_guidance(value: ContinuationGuidance) -> &'static str {
    match value {
        ContinuationGuidance::UseCursor => "use_cursor",
        ContinuationGuidance::NarrowScope => "narrow_scope",
        ContinuationGuidance::SplitRequest => "split_request",
        ContinuationGuidance::ReduceDepth => "reduce_depth",
        ContinuationGuidance::ReduceRelations => "reduce_relations",
        ContinuationGuidance::RequestSource => "request_source",
        ContinuationGuidance::IncreaseBudgetWithinLimit => "increase_budget_within_limit",
        ContinuationGuidance::RefreshCoverage => "refresh_coverage",
        ContinuationGuidance::UnsupportedNoContinuation => "unsupported_no_continuation",
    }
}

const fn limiting_resource(value: LimitingResourceKind) -> &'static str {
    match value {
        LimitingResourceKind::Rows => "rows",
        LimitingResourceKind::Edges => "edges",
        LimitingResourceKind::Results => "results",
        LimitingResourceKind::Depth => "depth",
        LimitingResourceKind::Paths => "paths",
        LimitingResourceKind::SourceBytes => "source_bytes",
        LimitingResourceKind::ResponseBytes => "response_bytes",
        LimitingResourceKind::MemoryBytes => "memory_bytes",
        LimitingResourceKind::Deadline => "deadline",
        LimitingResourceKind::EstimatedTokens => "estimated_tokens",
        LimitingResourceKind::Cancellation => "cancellation",
        LimitingResourceKind::Capability => "capability",
        LimitingResourceKind::Coverage => "coverage",
        LimitingResourceKind::PageSize => "page_size",
    }
}

fn graph_registry_error(error: GraphRegistryError) -> ApiError {
    match error {
        GraphRegistryError::Invalid | GraphRegistryError::Exhausted => {
            ApiError::graph_projection_not_found()
        }
        GraphRegistryError::LimitReached => ApiError::graph_projection_limit_reached(),
        GraphRegistryError::Busy | GraphRegistryError::OrdinalOverflow => {
            ApiError::graph_projection_conflict()
        }
        GraphRegistryError::ResourceUnavailable => ApiError::daemon_unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_request_requires_exact_identity_scope_and_bounded_confidence() {
        let repository = RepositoryId::from_bytes([1; 16]).to_string();
        let generation = GenerationId::from_bytes([2; 20]).to_string();
        assert!(
            checked_request(OpenGraphRequest {
                repository_id: repository.clone(),
                generation_id: generation.clone(),
                view: BrowserGraphView::Architecture,
                symbol_ids: Vec::new(),
                relations: Vec::new(),
                min_confidence: 750,
                budget_profile: BrowserBudgetProfile::Balanced,
            })
            .is_ok()
        );
        assert!(
            checked_request(OpenGraphRequest {
                repository_id: repository.clone(),
                generation_id: "active".to_owned(),
                view: BrowserGraphView::Architecture,
                symbol_ids: Vec::new(),
                relations: Vec::new(),
                min_confidence: 0,
                budget_profile: BrowserBudgetProfile::Balanced,
            })
            .is_err()
        );
        assert!(
            checked_request(OpenGraphRequest {
                repository_id: repository,
                generation_id: generation,
                view: BrowserGraphView::Files,
                symbol_ids: vec!["symbol-in-whole-repository".to_owned()],
                relations: Vec::new(),
                min_confidence: 1_001,
                budget_profile: BrowserBudgetProfile::Expanded,
            })
            .is_err()
        );
    }

    #[test]
    fn unknown_graph_values_have_explicit_browser_fallbacks() {
        assert_eq!(node_kind(GraphNodeKind::Unknown(90)), "unknown");
        assert_eq!(relation_kind(GraphRelationKind::Unknown(91)), "unknown");
        assert_eq!(evidence_class(GraphEvidenceClass::Unknown(92)), "unknown");
        assert_eq!(overlay_role(GraphOverlayRole::Unknown(93)), "unknown");
    }
}
