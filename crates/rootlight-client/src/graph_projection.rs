//! Typed client contract for bounded, exact-generation graph projections.
//!
//! The parser validates global ordinals, checksums, cursor progression, and
//! response correlation before exposing source-free graph records.

use std::collections::BTreeSet;

use rootlight_protocol::{
    MAX_GRAPH_AGGREGATE_EDGES, MAX_GRAPH_AGGREGATE_NODES, MAX_GRAPH_PAGE_EDGES,
    MAX_GRAPH_PAGE_NODES,
    generated::{daemon::v1 as daemon, ui::graph::v1 as wire_graph},
    validate_graph_page,
};

use super::{
    Client, ClientError, GenerationId, GenerationSelector, QueryContext, RepositoryId,
    RequestOptions, RequestTimeout, ResultCompleteness, SymbolId, first_slice_schema,
    generation_to_wire, parse_query_context, parse_result_completeness, repository_to_wire,
    require_first_slice_response_schema, symbol_to_wire,
};

const MAX_GRAPH_SYMBOL_SEEDS: usize = 64;
const MAX_GRAPH_RELATIONS: usize = 16;
const GRAPH_PROJECTION_ID_BYTES: usize = 16;
const GRAPH_CURSOR_BYTES: usize = 32;

/// Closed graph views supported by the first bounded projection contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphProjectionView {
    /// File-granularity architecture with aggregate connections and communities.
    Architecture,
    /// File-granularity structural graph without derived architecture overlays.
    Files,
    /// Direct typed relationships around explicit symbol seeds.
    Symbols,
    /// Direct one-hop neighborhood around explicit symbol seeds.
    Neighborhood,
}

impl GraphProjectionView {
    const fn to_wire(self) -> i32 {
        match self {
            Self::Architecture => wire_graph::ProjectionView::Architecture as i32,
            Self::Files => wire_graph::ProjectionView::Files as i32,
            Self::Symbols => wire_graph::ProjectionView::Symbols as i32,
            Self::Neighborhood => wire_graph::ProjectionView::Neighborhood as i32,
        }
    }

    const fn accepts_whole_repository(self) -> bool {
        matches!(self, Self::Architecture | Self::Files)
    }

    const fn accepts_symbols(self) -> bool {
        matches!(self, Self::Symbols | Self::Neighborhood)
    }
}

/// Stable relation family carried by graph edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphRelationKind {
    /// Direct call from source to target.
    Calls,
    /// Reverse call relation.
    CalledBy,
    /// Structural reference.
    References,
    /// Type relation.
    Types,
    /// Interface or trait implementation.
    Implements,
    /// Module or package import.
    Imports,
    /// Test coverage or selection relation.
    Tests,
    /// Ownership relation.
    Ownership,
    /// Cross-service call.
    ServiceCall,
    /// Call into a route.
    CallsRoute,
    /// Messaging relation.
    Messaging,
    /// Table read.
    ReadsTable,
    /// Table write.
    WritesTable,
    /// Build dependency.
    BuildDependency,
    /// Data-flow relation.
    DataFlow,
    /// Historical relation.
    History,
    /// Additive value introduced by a newer compatible daemon.
    Unknown(i32),
}

impl GraphRelationKind {
    fn to_request_wire(self) -> Result<i32, ClientError> {
        let value = match self {
            Self::Calls => wire_graph::RelationKind::Calls,
            Self::CalledBy => wire_graph::RelationKind::CalledBy,
            Self::References => wire_graph::RelationKind::References,
            Self::Types => wire_graph::RelationKind::Types,
            Self::Implements => wire_graph::RelationKind::Implements,
            Self::Imports => wire_graph::RelationKind::Imports,
            Self::Tests => wire_graph::RelationKind::Tests,
            Self::Ownership => wire_graph::RelationKind::Ownership,
            Self::ServiceCall => wire_graph::RelationKind::ServiceCall,
            Self::CallsRoute => wire_graph::RelationKind::CallsRoute,
            Self::Messaging => wire_graph::RelationKind::Messaging,
            Self::ReadsTable => wire_graph::RelationKind::ReadsTable,
            Self::WritesTable => wire_graph::RelationKind::WritesTable,
            Self::BuildDependency => wire_graph::RelationKind::BuildDependency,
            Self::DataFlow => wire_graph::RelationKind::DataFlow,
            Self::History => wire_graph::RelationKind::History,
            Self::Unknown(_) => return Err(ClientError::InvalidFirstSliceRequest),
        };
        Ok(value as i32)
    }

    fn from_wire(value: i32) -> Self {
        match wire_graph::RelationKind::try_from(value) {
            Ok(wire_graph::RelationKind::Calls) => Self::Calls,
            Ok(wire_graph::RelationKind::CalledBy) => Self::CalledBy,
            Ok(wire_graph::RelationKind::References) => Self::References,
            Ok(wire_graph::RelationKind::Types) => Self::Types,
            Ok(wire_graph::RelationKind::Implements) => Self::Implements,
            Ok(wire_graph::RelationKind::Imports) => Self::Imports,
            Ok(wire_graph::RelationKind::Tests) => Self::Tests,
            Ok(wire_graph::RelationKind::Ownership) => Self::Ownership,
            Ok(wire_graph::RelationKind::ServiceCall) => Self::ServiceCall,
            Ok(wire_graph::RelationKind::CallsRoute) => Self::CallsRoute,
            Ok(wire_graph::RelationKind::Messaging) => Self::Messaging,
            Ok(wire_graph::RelationKind::ReadsTable) => Self::ReadsTable,
            Ok(wire_graph::RelationKind::WritesTable) => Self::WritesTable,
            Ok(wire_graph::RelationKind::BuildDependency) => Self::BuildDependency,
            Ok(wire_graph::RelationKind::DataFlow) => Self::DataFlow,
            Ok(wire_graph::RelationKind::History) => Self::History,
            Ok(wire_graph::RelationKind::Unspecified) | Err(_) => Self::Unknown(value),
        }
    }
}

/// Stable identity domain carried by a graph node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeIdKind {
    /// Stable file identity.
    File,
    /// Stable symbol identity.
    Symbol,
    /// Additive value introduced by a newer compatible daemon.
    Unknown(i32),
}

/// Stable graph node category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeKind {
    /// File node.
    File,
    /// Symbol node.
    Symbol,
    /// Additive value introduced by a newer compatible daemon.
    Unknown(i32),
}

/// Strength and derivation class of one graph record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEvidenceClass {
    /// Direct structural evidence.
    Structural,
    /// Aggregate evidence derived from several structural records.
    Aggregated,
    /// Candidate evidence that is intentionally weaker than structural truth.
    Candidate,
    /// Additive value introduced by a newer compatible daemon.
    Unknown(i32),
}

/// Optional additive annotation role for a graph edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphOverlayRole {
    /// No overlay annotation.
    None,
    /// Additive value introduced by a newer compatible daemon.
    Unknown(i32),
}

/// Checked caller-requested page and retained-snapshot limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct GraphProjectionBudget {
    page_nodes: u32,
    page_edges: u32,
    aggregate_nodes: u32,
    aggregate_edges: u32,
}

impl GraphProjectionBudget {
    /// Creates non-zero limits whose aggregate bounds cover one page.
    ///
    /// Values above current server caps are permitted and are clamped
    /// explicitly in [`GraphProjectionPage::effective_budget`].
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidFirstSliceRequest`] for zero limits or
    /// when a page limit exceeds its aggregate limit.
    pub fn new(
        page_nodes: u32,
        page_edges: u32,
        aggregate_nodes: u32,
        aggregate_edges: u32,
    ) -> Result<Self, ClientError> {
        if page_nodes == 0
            || page_edges == 0
            || aggregate_nodes == 0
            || aggregate_edges == 0
            || page_nodes > aggregate_nodes
            || page_edges > aggregate_edges
        {
            return Err(ClientError::InvalidFirstSliceRequest);
        }
        Ok(Self {
            page_nodes,
            page_edges,
            aggregate_nodes,
            aggregate_edges,
        })
    }

    /// Returns the requested node limit for one page.
    #[must_use]
    pub const fn page_nodes(self) -> u32 {
        self.page_nodes
    }

    /// Returns the requested edge limit for one page.
    #[must_use]
    pub const fn page_edges(self) -> u32 {
        self.page_edges
    }

    /// Returns the requested retained node limit.
    #[must_use]
    pub const fn aggregate_nodes(self) -> u32 {
        self.aggregate_nodes
    }

    /// Returns the requested retained edge limit.
    #[must_use]
    pub const fn aggregate_edges(self) -> u32 {
        self.aggregate_edges
    }

    const fn to_wire(self) -> daemon::GraphProjectionBudget {
        daemon::GraphProjectionBudget {
            page_nodes: self.page_nodes,
            page_edges: self.page_edges,
            aggregate_nodes: self.aggregate_nodes,
            aggregate_edges: self.aggregate_edges,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GraphProjectionScope {
    WholeRepository,
    Symbols(Vec<SymbolId>),
}

/// Checked open request for one exact immutable generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphProjectionRequest {
    repository: RepositoryId,
    generation: GenerationId,
    view: GraphProjectionView,
    scope: GraphProjectionScope,
    relations: Vec<GraphRelationKind>,
    min_confidence: u32,
    budget: GraphProjectionBudget,
}

impl GraphProjectionRequest {
    /// Creates a whole-repository architecture or file projection.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidFirstSliceRequest`] when `view` requires
    /// explicit symbol seeds.
    pub fn whole_repository(
        repository: RepositoryId,
        generation: GenerationId,
        view: GraphProjectionView,
        budget: GraphProjectionBudget,
    ) -> Result<Self, ClientError> {
        if !view.accepts_whole_repository() {
            return Err(ClientError::InvalidFirstSliceRequest);
        }
        Ok(Self {
            repository,
            generation,
            view,
            scope: GraphProjectionScope::WholeRepository,
            relations: Vec::new(),
            min_confidence: 0,
            budget,
        })
    }

    /// Creates a direct symbol or neighborhood projection.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidFirstSliceRequest`] for a mismatched view,
    /// empty or over-limit seeds and relations, duplicate values, or an unknown
    /// relation that cannot be sent to the older wire contract.
    pub fn symbols(
        repository: RepositoryId,
        generation: GenerationId,
        view: GraphProjectionView,
        symbols: &[SymbolId],
        relations: &[GraphRelationKind],
        budget: GraphProjectionBudget,
    ) -> Result<Self, ClientError> {
        if !view.accepts_symbols()
            || symbols.is_empty()
            || symbols.len() > MAX_GRAPH_SYMBOL_SEEDS
            || relations.is_empty()
            || relations.len() > MAX_GRAPH_RELATIONS
        {
            return Err(ClientError::InvalidFirstSliceRequest);
        }
        let symbol_set = symbols.iter().copied().collect::<BTreeSet<_>>();
        let relation_set = relations.iter().copied().collect::<BTreeSet<_>>();
        if symbol_set.len() != symbols.len()
            || relation_set.len() != relations.len()
            || relations
                .iter()
                .copied()
                .any(|relation| relation.to_request_wire().is_err())
        {
            return Err(ClientError::InvalidFirstSliceRequest);
        }
        Ok(Self {
            repository,
            generation,
            view,
            scope: GraphProjectionScope::Symbols(symbols.to_vec()),
            relations: relations.to_vec(),
            min_confidence: 0,
            budget,
        })
    }

    /// Applies a closed confidence floor in thousandths.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidFirstSliceRequest`] above 1,000.
    pub fn with_min_confidence(mut self, min_confidence: u32) -> Result<Self, ClientError> {
        if min_confidence > 1_000 {
            return Err(ClientError::InvalidFirstSliceRequest);
        }
        self.min_confidence = min_confidence;
        Ok(self)
    }

    /// Returns the repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the exact immutable generation identity.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the selected graph view.
    #[must_use]
    pub const fn view(&self) -> GraphProjectionView {
        self.view
    }

    /// Returns the requested graph budget.
    #[must_use]
    pub const fn budget(&self) -> GraphProjectionBudget {
        self.budget
    }
}

/// Opaque identifier for one owner-bound retained projection.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct GraphProjectionId([u8; GRAPH_PROJECTION_ID_BYTES]);

impl std::fmt::Debug for GraphProjectionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GraphProjectionId(<opaque>)")
    }
}

/// Server-clamped page and retained-snapshot limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct GraphProjectionEffectiveBudget {
    /// Maximum nodes in one page.
    pub page_nodes: u32,
    /// Maximum edges in one page.
    pub page_edges: u32,
    /// Maximum retained nodes.
    pub aggregate_nodes: u32,
    /// Maximum retained edges.
    pub aggregate_edges: u32,
}

/// Owner-bound sequential continuation for one graph projection.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphProjectionContinuation {
    projection: GraphProjectionId,
    cursor: [u8; GRAPH_CURSOR_BYTES],
    repository: RepositoryId,
    generation: GenerationId,
    returned_nodes: u64,
    returned_edges: u64,
    total_matching_nodes: u64,
    total_matching_edges: u64,
    effective_budget: GraphProjectionEffectiveBudget,
}

impl std::fmt::Debug for GraphProjectionContinuation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphProjectionContinuation")
            .field("projection", &self.projection)
            .field("returned_nodes", &self.returned_nodes)
            .field("returned_edges", &self.returned_edges)
            .finish_non_exhaustive()
    }
}

impl GraphProjectionContinuation {
    /// Returns the retained projection identity.
    #[must_use]
    pub const fn projection(&self) -> GraphProjectionId {
        self.projection
    }
}

/// One validated source-free node with page-local strings resolved.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphNode {
    /// Projection-global ordinal.
    pub ordinal: u32,
    /// Stable opaque identity.
    pub stable_id: String,
    /// Identity domain.
    pub id_kind: GraphNodeIdKind,
    /// Human-readable bounded label.
    pub label: String,
    /// Optional repository-relative path.
    pub path: Option<String>,
    /// Node category.
    pub kind: GraphNodeKind,
    /// Confidence in thousandths.
    pub confidence: u32,
    /// Generated-file classification when authoritative.
    pub generated: Option<bool>,
    /// Optional derived community identifier.
    pub community: Option<String>,
    /// Optional architecture component identifier.
    pub component: Option<String>,
    /// Optional number of contained symbols.
    pub symbol_count: Option<u32>,
    /// Optional incoming edge count.
    pub fan_in: Option<u32>,
    /// Optional outgoing edge count.
    pub fan_out: Option<u32>,
    /// Optional structural hotspot score.
    pub hotspot_score: Option<u32>,
    /// Evidence derivation class.
    pub evidence: GraphEvidenceClass,
}

/// One validated source-free edge over projection-global ordinals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct GraphEdge {
    /// Source node ordinal.
    pub source_ordinal: u32,
    /// Target node ordinal.
    pub target_ordinal: u32,
    /// Typed relation family.
    pub relation: GraphRelationKind,
    /// Aggregate relation weight.
    pub weight: u32,
    /// Confidence in thousandths.
    pub confidence: u32,
    /// Whether the relation is exact.
    pub exact: bool,
    /// Whether the relation is inferred.
    pub inferred: bool,
    /// Number of retained source records without source disclosure.
    pub evidence_count: u32,
    /// Optional additive overlay role.
    pub overlay: GraphOverlayRole,
}

/// One checked page in a retained exact-generation projection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphProjectionPage {
    /// Retained projection identity.
    pub projection: GraphProjectionId,
    /// Authoritative repository, generation, freshness, and usage context.
    pub context: QueryContext,
    /// Source-free nodes newly introduced by this page.
    pub nodes: Vec<GraphNode>,
    /// Source-free edges whose endpoints are already known.
    pub edges: Vec<GraphEdge>,
    /// Authoritative completeness for this page boundary.
    pub completeness: ResultCompleteness,
    /// Server-clamped projection limits.
    pub effective_budget: GraphProjectionEffectiveBudget,
    /// Nodes returned through this page.
    pub returned_nodes_cumulative: u64,
    /// Edges returned through this page.
    pub returned_edges_cumulative: u64,
    /// Nodes retained in the bounded projection.
    pub total_matching_nodes: u64,
    /// Edges retained in the bounded projection.
    pub total_matching_edges: u64,
    /// Total authoritative nodes when the producer established it.
    pub total_known_nodes: Option<u64>,
    /// Total authoritative edges when the producer established it.
    pub total_known_edges: Option<u64>,
    /// Edges omitted because a bounded endpoint was unavailable.
    pub edges_omitted_for_unavailable_endpoints: u64,
    /// Records skipped because authoritative coverage was unavailable.
    pub skipped_for_coverage: u64,
    /// Checked owner-bound cursor for the next sequential page.
    pub continuation: Option<GraphProjectionContinuation>,
}

impl Client {
    /// Opens a bounded graph projection and returns its first page.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when request validation, transport, negotiation,
    /// daemon execution, or checked response decoding fails.
    pub fn graph_projection_open(
        &self,
        request: &GraphProjectionRequest,
    ) -> Result<GraphProjectionPage, ClientError> {
        self.graph_projection_open_with_options(request, RequestOptions::new())
    }

    /// Opens a bounded graph projection with explicit transport options.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when request validation, transport, negotiation,
    /// daemon execution, or checked response decoding fails.
    pub fn graph_projection_open_with_options(
        &self,
        request: &GraphProjectionRequest,
        options: RequestOptions,
    ) -> Result<GraphProjectionPage, ClientError> {
        match self.request_with_options(build_open_request(request)?, options)? {
            daemon::response_envelope::Response::GraphProjection(response) => {
                parse_graph_projection_response(
                    response,
                    request.repository,
                    request.generation,
                    None,
                )
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Opens a bounded graph projection within an explicit async deadline.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when request validation, transport, negotiation,
    /// deadline enforcement, daemon execution, or checked response decoding fails.
    pub async fn graph_projection_open_async(
        &self,
        request: &GraphProjectionRequest,
        timeout: RequestTimeout,
    ) -> Result<GraphProjectionPage, ClientError> {
        self.graph_projection_open_async_with_options(
            request,
            RequestOptions::new().with_timeout(timeout),
        )
        .await
    }

    /// Opens a bounded graph projection asynchronously with transport options.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when request validation, transport, negotiation,
    /// deadline enforcement, daemon execution, or checked response decoding fails.
    pub async fn graph_projection_open_async_with_options(
        &self,
        request: &GraphProjectionRequest,
        options: RequestOptions,
    ) -> Result<GraphProjectionPage, ClientError> {
        match self
            .request_async_with_options(build_open_request(request)?, options)
            .await?
        {
            daemon::response_envelope::Response::GraphProjection(response) => {
                parse_graph_projection_response(
                    response,
                    request.repository,
                    request.generation,
                    None,
                )
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Consumes one single-use continuation and returns the next graph page.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, negotiation, owner-bound cursor
    /// validation, daemon execution, or checked response decoding fails.
    pub fn graph_projection_page(
        &self,
        continuation: &GraphProjectionContinuation,
    ) -> Result<GraphProjectionPage, ClientError> {
        self.graph_projection_page_with_options(continuation, RequestOptions::new())
    }

    /// Consumes one graph continuation with explicit transport options.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, negotiation, owner-bound cursor
    /// validation, daemon execution, or checked response decoding fails.
    pub fn graph_projection_page_with_options(
        &self,
        continuation: &GraphProjectionContinuation,
        options: RequestOptions,
    ) -> Result<GraphProjectionPage, ClientError> {
        match self.request_with_options(build_page_request(continuation), options)? {
            daemon::response_envelope::Response::GraphProjection(response) => {
                parse_graph_projection_response(
                    response,
                    continuation.repository,
                    continuation.generation,
                    Some(continuation),
                )
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Consumes one graph continuation within an explicit async deadline.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, negotiation, deadline enforcement,
    /// owner-bound cursor validation, daemon execution, or response decoding fails.
    pub async fn graph_projection_page_async(
        &self,
        continuation: &GraphProjectionContinuation,
        timeout: RequestTimeout,
    ) -> Result<GraphProjectionPage, ClientError> {
        self.graph_projection_page_async_with_options(
            continuation,
            RequestOptions::new().with_timeout(timeout),
        )
        .await
    }

    /// Consumes one graph continuation asynchronously with transport options.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, negotiation, deadline enforcement,
    /// owner-bound cursor validation, daemon execution, or response decoding fails.
    pub async fn graph_projection_page_async_with_options(
        &self,
        continuation: &GraphProjectionContinuation,
        options: RequestOptions,
    ) -> Result<GraphProjectionPage, ClientError> {
        match self
            .request_async_with_options(build_page_request(continuation), options)
            .await?
        {
            daemon::response_envelope::Response::GraphProjection(response) => {
                parse_graph_projection_response(
                    response,
                    continuation.repository,
                    continuation.generation,
                    Some(continuation),
                )
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Explicitly releases a retained graph projection.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, negotiation, daemon execution, or
    /// release-response correlation fails.
    pub fn graph_projection_release(
        &self,
        projection: GraphProjectionId,
    ) -> Result<bool, ClientError> {
        self.graph_projection_release_with_options(projection, RequestOptions::new())
    }

    /// Releases a retained graph projection with explicit transport options.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, negotiation, daemon execution, or
    /// release-response correlation fails.
    pub fn graph_projection_release_with_options(
        &self,
        projection: GraphProjectionId,
        options: RequestOptions,
    ) -> Result<bool, ClientError> {
        match self.request_with_options(build_release_request(projection), options)? {
            daemon::response_envelope::Response::GraphProjectionRelease(response) => {
                parse_release_response(response, projection)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Releases a retained graph projection within an explicit async deadline.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, negotiation, deadline enforcement,
    /// daemon execution, or release-response correlation fails.
    pub async fn graph_projection_release_async(
        &self,
        projection: GraphProjectionId,
        timeout: RequestTimeout,
    ) -> Result<bool, ClientError> {
        self.graph_projection_release_async_with_options(
            projection,
            RequestOptions::new().with_timeout(timeout),
        )
        .await
    }

    /// Releases a retained graph projection asynchronously with transport options.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, negotiation, deadline enforcement,
    /// daemon execution, or release-response correlation fails.
    pub async fn graph_projection_release_async_with_options(
        &self,
        projection: GraphProjectionId,
        options: RequestOptions,
    ) -> Result<bool, ClientError> {
        match self
            .request_async_with_options(build_release_request(projection), options)
            .await?
        {
            daemon::response_envelope::Response::GraphProjectionRelease(response) => {
                parse_release_response(response, projection)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }
}

fn build_open_request(
    request: &GraphProjectionRequest,
) -> Result<daemon::request_envelope::Request, ClientError> {
    let scope = match &request.scope {
        GraphProjectionScope::WholeRepository => {
            daemon::graph_projection_scope::Scope::WholeRepository(
                daemon::GraphProjectionWholeRepository {},
            )
        }
        GraphProjectionScope::Symbols(symbols) => {
            daemon::graph_projection_scope::Scope::Symbols(daemon::GraphProjectionSymbolScope {
                symbols: symbols.iter().copied().map(symbol_to_wire).collect(),
            })
        }
    };
    let relations = request
        .relations
        .iter()
        .copied()
        .map(GraphRelationKind::to_request_wire)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(daemon::request_envelope::Request::GraphProjectionOpen(
        daemon::GraphProjectionOpenRequest {
            schema_version: Some(first_slice_schema()),
            repository: Some(repository_to_wire(request.repository)),
            generation: Some(generation_to_wire(request.generation)),
            view: request.view.to_wire(),
            scope: Some(daemon::GraphProjectionScope { scope: Some(scope) }),
            filters: Some(daemon::GraphProjectionFilters {
                node_kinds: Vec::new(),
                relations,
                languages: Vec::new(),
                min_confidence: request.min_confidence,
                include_inferred: Some(false),
                include_generated: Some(true),
                community_id: None,
                hotspot_threshold: None,
            }),
            budget: Some(request.budget.to_wire()),
        },
    ))
}

fn build_page_request(
    continuation: &GraphProjectionContinuation,
) -> daemon::request_envelope::Request {
    daemon::request_envelope::Request::GraphProjectionPage(daemon::GraphProjectionPageRequest {
        schema_version: Some(first_slice_schema()),
        projection_id: continuation.projection.0.to_vec(),
        cursor: continuation.cursor.to_vec(),
    })
}

fn build_release_request(projection: GraphProjectionId) -> daemon::request_envelope::Request {
    daemon::request_envelope::Request::GraphProjectionRelease(
        daemon::GraphProjectionReleaseRequest {
            schema_version: Some(first_slice_schema()),
            projection_id: projection.0.to_vec(),
        },
    )
}

fn parse_release_response(
    response: daemon::GraphProjectionReleaseResponse,
    expected: GraphProjectionId,
) -> Result<bool, ClientError> {
    require_first_slice_response_schema(response.schema_version)?;
    if parse_projection_id(response.projection_id)? != expected {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(response.released)
}

fn parse_graph_projection_response(
    response: daemon::GraphProjectionResponse,
    repository: RepositoryId,
    generation: GenerationId,
    previous: Option<&GraphProjectionContinuation>,
) -> Result<GraphProjectionPage, ClientError> {
    require_first_slice_response_schema(response.schema_version)?;
    let projection = parse_projection_id(response.projection_id)?;
    if previous.is_some_and(|previous| previous.projection != projection) {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let context = parse_query_context(
        response.context,
        repository,
        GenerationSelector::Generation(generation),
    )?;
    let effective_budget = parse_effective_budget(
        response
            .effective_budget
            .ok_or(ClientError::InvalidResponseCorrelation)?,
    )?;
    if previous.is_some_and(|previous| previous.effective_budget != effective_budget) {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let page = response
        .page
        .ok_or(ClientError::InvalidResponseCorrelation)?;
    validate_graph_page(&page).map_err(|_| ClientError::InvalidResponseCorrelation)?;

    let previous_nodes = previous.map_or(0, |value| value.returned_nodes);
    let previous_edges = previous.map_or(0, |value| value.returned_edges);
    if page.returned_nodes_cumulative
        != previous_nodes
            .checked_add(
                u64::try_from(page.nodes.len())
                    .map_err(|_| ClientError::InvalidResponseCorrelation)?,
            )
            .ok_or(ClientError::InvalidResponseCorrelation)?
        || page.returned_edges_cumulative
            != previous_edges
                .checked_add(
                    u64::try_from(page.edges.len())
                        .map_err(|_| ClientError::InvalidResponseCorrelation)?,
                )
                .ok_or(ClientError::InvalidResponseCorrelation)?
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    for (index, node) in page.nodes.iter().enumerate() {
        let expected = previous_nodes
            .checked_add(u64::try_from(index).map_err(|_| ClientError::InvalidResponseCorrelation)?)
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        if u64::from(node.ordinal) != expected {
            return Err(ClientError::InvalidResponseCorrelation);
        }
    }

    let total_matching_nodes = page
        .total_matching_nodes
        .ok_or(ClientError::InvalidResponseCorrelation)?;
    let total_matching_edges = page
        .total_matching_edges
        .ok_or(ClientError::InvalidResponseCorrelation)?;
    if total_matching_nodes < page.returned_nodes_cumulative
        || total_matching_edges < page.returned_edges_cumulative
        || previous.is_some_and(|previous| {
            previous.total_matching_nodes != total_matching_nodes
                || previous.total_matching_edges != total_matching_edges
        })
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    if page
        .total_known_nodes
        .is_some_and(|total| total < total_matching_nodes)
        || page
            .total_known_edges
            .is_some_and(|total| total < total_matching_edges)
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }

    let next_cursor = response
        .next_cursor
        .map(|cursor| {
            <[u8; GRAPH_CURSOR_BYTES]>::try_from(cursor)
                .map_err(|_| ClientError::InvalidResponseCorrelation)
        })
        .transpose()?;
    if next_cursor.is_some()
        != (page.returned_nodes_cumulative < total_matching_nodes
            || page.returned_edges_cumulative < total_matching_edges)
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let completeness =
        parse_result_completeness(response.completeness, None, next_cursor.is_some())?;

    let mut nodes = Vec::new();
    nodes
        .try_reserve_exact(page.nodes.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    for node in page.nodes {
        nodes.push(parse_graph_node(node, &page.strings)?);
    }
    let edges = page.edges.into_iter().map(parse_graph_edge).collect();
    let continuation = next_cursor.map(|cursor| GraphProjectionContinuation {
        projection,
        cursor,
        repository,
        generation,
        returned_nodes: page.returned_nodes_cumulative,
        returned_edges: page.returned_edges_cumulative,
        total_matching_nodes,
        total_matching_edges,
        effective_budget,
    });
    Ok(GraphProjectionPage {
        projection,
        context,
        nodes,
        edges,
        completeness,
        effective_budget,
        returned_nodes_cumulative: page.returned_nodes_cumulative,
        returned_edges_cumulative: page.returned_edges_cumulative,
        total_matching_nodes,
        total_matching_edges,
        total_known_nodes: page.total_known_nodes,
        total_known_edges: page.total_known_edges,
        edges_omitted_for_unavailable_endpoints: page.edges_omitted_for_unavailable_endpoints,
        skipped_for_coverage: page.skipped_for_coverage,
        continuation,
    })
}

fn parse_effective_budget(
    budget: daemon::GraphProjectionEffectiveBudget,
) -> Result<GraphProjectionEffectiveBudget, ClientError> {
    if budget.page_nodes == 0
        || budget.page_nodes > u32::try_from(MAX_GRAPH_PAGE_NODES).unwrap_or(u32::MAX)
        || budget.page_edges == 0
        || budget.page_edges > u32::try_from(MAX_GRAPH_PAGE_EDGES).unwrap_or(u32::MAX)
        || budget.aggregate_nodes == 0
        || budget.aggregate_nodes > MAX_GRAPH_AGGREGATE_NODES
        || budget.aggregate_edges == 0
        || budget.aggregate_edges > MAX_GRAPH_AGGREGATE_EDGES
        || budget.page_nodes > budget.aggregate_nodes
        || budget.page_edges > budget.aggregate_edges
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(GraphProjectionEffectiveBudget {
        page_nodes: budget.page_nodes,
        page_edges: budget.page_edges,
        aggregate_nodes: budget.aggregate_nodes,
        aggregate_edges: budget.aggregate_edges,
    })
}

fn parse_graph_node(
    node: wire_graph::GraphNode,
    strings: &[String],
) -> Result<GraphNode, ClientError> {
    Ok(GraphNode {
        ordinal: node.ordinal,
        stable_id: node.stable_id,
        id_kind: match wire_graph::NodeIdKind::try_from(node.id_kind) {
            Ok(wire_graph::NodeIdKind::File) => GraphNodeIdKind::File,
            Ok(wire_graph::NodeIdKind::Symbol) => GraphNodeIdKind::Symbol,
            Ok(wire_graph::NodeIdKind::Unspecified) | Err(_) => {
                GraphNodeIdKind::Unknown(node.id_kind)
            }
        },
        label: resolve_required_string(strings, node.label_index)?,
        path: resolve_optional_string(strings, node.path_index)?,
        kind: match wire_graph::NodeKind::try_from(node.kind) {
            Ok(wire_graph::NodeKind::File) => GraphNodeKind::File,
            Ok(wire_graph::NodeKind::Symbol) => GraphNodeKind::Symbol,
            Ok(wire_graph::NodeKind::Unspecified) | Err(_) => GraphNodeKind::Unknown(node.kind),
        },
        confidence: node.confidence,
        generated: node.generated,
        community: resolve_optional_string(strings, node.community_index)?,
        component: resolve_optional_string(strings, node.component_index)?,
        symbol_count: node.symbol_count,
        fan_in: node.fan_in,
        fan_out: node.fan_out,
        hotspot_score: node.hotspot_score,
        evidence: match wire_graph::EvidenceClass::try_from(node.evidence) {
            Ok(wire_graph::EvidenceClass::Structural) => GraphEvidenceClass::Structural,
            Ok(wire_graph::EvidenceClass::Aggregated) => GraphEvidenceClass::Aggregated,
            Ok(wire_graph::EvidenceClass::Candidate) => GraphEvidenceClass::Candidate,
            Ok(wire_graph::EvidenceClass::Unspecified) | Err(_) => {
                GraphEvidenceClass::Unknown(node.evidence)
            }
        },
    })
}

fn parse_graph_edge(edge: wire_graph::GraphEdge) -> GraphEdge {
    GraphEdge {
        source_ordinal: edge.source_ordinal,
        target_ordinal: edge.target_ordinal,
        relation: GraphRelationKind::from_wire(edge.relation),
        weight: edge.weight,
        confidence: edge.confidence,
        exact: edge.exact,
        inferred: edge.inferred,
        evidence_count: edge.evidence_count,
        overlay: match wire_graph::OverlayRole::try_from(edge.overlay) {
            Ok(wire_graph::OverlayRole::None) => GraphOverlayRole::None,
            Ok(wire_graph::OverlayRole::Unspecified) | Err(_) => {
                GraphOverlayRole::Unknown(edge.overlay)
            }
        },
    }
}

fn resolve_required_string(strings: &[String], index: u32) -> Result<String, ClientError> {
    let index = usize::try_from(index).map_err(|_| ClientError::InvalidResponseCorrelation)?;
    strings
        .get(index)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or(ClientError::InvalidResponseCorrelation)
}

fn resolve_optional_string(
    strings: &[String],
    index: Option<u32>,
) -> Result<Option<String>, ClientError> {
    index
        .map(|index| resolve_required_string(strings, index))
        .transpose()
}

fn parse_projection_id(value: Vec<u8>) -> Result<GraphProjectionId, ClientError> {
    <[u8; GRAPH_PROJECTION_ID_BYTES]>::try_from(value)
        .map(GraphProjectionId)
        .map_err(|_| ClientError::InvalidResponseCorrelation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> GraphProjectionBudget {
        GraphProjectionBudget::new(10, 20, 100, 200).expect("valid graph budget")
    }

    #[test]
    fn request_builders_reject_scope_and_relation_ambiguity() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let symbol = SymbolId::from_bytes([3; 20]);

        assert!(matches!(
            GraphProjectionRequest::whole_repository(
                repository,
                generation,
                GraphProjectionView::Symbols,
                budget(),
            ),
            Err(ClientError::InvalidFirstSliceRequest)
        ));
        assert!(matches!(
            GraphProjectionRequest::symbols(
                repository,
                generation,
                GraphProjectionView::Neighborhood,
                &[symbol, symbol],
                &[GraphRelationKind::Calls],
                budget(),
            ),
            Err(ClientError::InvalidFirstSliceRequest)
        ));
        assert!(matches!(
            GraphProjectionRequest::symbols(
                repository,
                generation,
                GraphProjectionView::Neighborhood,
                &[symbol],
                &[GraphRelationKind::Unknown(99)],
                budget(),
            ),
            Err(ClientError::InvalidFirstSliceRequest)
        ));
    }

    #[test]
    fn response_enums_preserve_unknown_additive_values() {
        let node = parse_graph_node(
            wire_graph::GraphNode {
                ordinal: 0,
                stable_id: "node".to_owned(),
                id_kind: 41,
                label_index: 1,
                path_index: None,
                kind: 42,
                confidence: 1_000,
                generated: None,
                community_index: None,
                component_index: None,
                symbol_count: None,
                fan_in: None,
                fan_out: None,
                hotspot_score: None,
                evidence: 43,
            },
            &[String::new(), "label".to_owned()],
        )
        .expect("unknown non-zero values remain readable");

        assert_eq!(node.id_kind, GraphNodeIdKind::Unknown(41));
        assert_eq!(node.kind, GraphNodeKind::Unknown(42));
        assert_eq!(node.evidence, GraphEvidenceClass::Unknown(43));
        let edge = parse_graph_edge(wire_graph::GraphEdge {
            source_ordinal: 0,
            target_ordinal: 0,
            relation: 44,
            weight: 1,
            confidence: 1_000,
            exact: false,
            inferred: false,
            evidence_count: 1,
            overlay: 45,
        });
        assert_eq!(edge.relation, GraphRelationKind::Unknown(44));
        assert_eq!(edge.overlay, GraphOverlayRole::Unknown(45));
    }

    #[test]
    fn effective_budget_rejects_server_values_above_caps() {
        assert!(matches!(
            parse_effective_budget(daemon::GraphProjectionEffectiveBudget {
                page_nodes: 1,
                page_edges: 1,
                aggregate_nodes: MAX_GRAPH_AGGREGATE_NODES + 1,
                aggregate_edges: 1,
            }),
            Err(ClientError::InvalidResponseCorrelation)
        ));
    }
}
