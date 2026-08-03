//! Generation-pinned evidence, relationship, source, and change-impact routes.
//!
//! These handlers compose typed daemon intents into browser-safe DTOs. Source
//! bodies cross the boundary only through the explicit capability read route.

use std::{str::FromStr as _, time::Duration, time::Instant};

use axum::{
    Json,
    extract::{Extension, Path, RawQuery, State},
    http::{
        HeaderValue,
        header::{CACHE_CONTROL, PRAGMA, X_CONTENT_TYPE_OPTIONS},
    },
    response::{IntoResponse as _, Response},
};
use data_encoding::BASE64;
use rootlight_client::{
    AnalysisTier, ChangeImpact, ContinuationAvailability, ContinuationGuidance, CoverageStatus,
    GenerationId, GenerationSelector, LimitingResourceKind, QueryContext, QueryFreshness,
    RelationshipGroup, RelationshipTarget, RepositoryId, RequestTimeout, ResultCompleteness,
    ResultCompletenessState, SourceChunk, SourceEncoding, SourceRead, SourceReadOptions, SymbolId,
    SymbolRelationships, TokenAccountingProfile,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::{ApiError, AppState},
    session::AuthenticatedSession,
    source_registry::{IssuedSourceCapability, SourceRegistryError},
};

const EVIDENCE_TIMEOUT: Duration = Duration::from_secs(5);
const SOURCE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_DETAIL_QUERY_BYTES: usize = 512;
const MAX_DETAIL_QUERY_PARAMETERS: usize = 2;
const MAX_RELATION_SEEDS: usize = 8;
const MAX_RELATION_FAMILIES: usize = 16;
const MAX_RELATION_RESULTS: u16 = 100;
const MAX_RELATION_PAGE_OFFSET: u64 = 10_000;
const MAX_SOURCE_CAPABILITIES_PER_RESPONSE: usize = 64;
const MAX_SOURCE_CONTEXT_LINES: u8 = 8;
const MAX_SOURCE_BYTES: u64 = 64 * 1024;
const MAX_SOURCE_PATH_BYTES: usize = 8_192;
const MAX_IMPACT_SEEDS: usize = 16;
const MAX_IMPACT_DEPENDENTS: u16 = 200;
const MAX_IMPACT_TESTS: usize = 500;
const MAX_RESPONSE_LABEL_BYTES: usize = 256;
const MAX_DISPLAY_NAME_BYTES: usize = 512;
const MAX_SIGNATURE_BYTES: usize = 4_096;

const RELATION_ALLOWLIST: &[&str] = &[
    "calls",
    "called_by",
    "references",
    "types",
    "implements",
    "imports",
    "tests",
    "ownership",
    "service_call",
    "calls_route",
    "messaging",
    "reads_table",
    "writes_table",
    "build_dependency",
    "data_flow",
    "history",
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NodeDetailResponse {
    schema: &'static str,
    repository_id: String,
    generation_id: String,
    node_id: String,
    id_kind: &'static str,
    kind: String,
    display_name: String,
    qualified_name: Option<String>,
    signature: Option<String>,
    language: String,
    tier: &'static str,
    confidence: u32,
    provider: String,
    evidence: String,
    outbound_exact: String,
    outbound_candidates: String,
    inbound_exact: String,
    inbound_candidates: String,
    reference_count: String,
    generated: Option<bool>,
    source_references: Vec<SourceCapabilityDto>,
    context: QueryContextDto,
    completeness: CompletenessDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceCapabilityDto {
    capability: String,
    expires_in_seconds: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryContextDto {
    repository_id: String,
    generation_id: String,
    parent_generation_id: Option<String>,
    active_generation: bool,
    structural_freshness: &'static str,
    semantic_freshness: &'static str,
    tier: &'static str,
    coverage_status: &'static str,
    skipped_inputs: String,
    usage: QueryUsageDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryUsageDto {
    rows: String,
    edges: String,
    results: String,
    source_bytes: String,
    json_bytes: String,
    estimated_tokens: String,
    token_accounting_profile: Option<&'static str>,
    memory_bytes: Option<String>,
    elapsed_micros: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompletenessDto {
    state: &'static str,
    limiting_resources: Vec<LimitingResourceDto>,
    continuation: &'static str,
    guidance: Vec<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LimitingResourceDto {
    kind: &'static str,
    limit: Option<String>,
    observed: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RelationshipsRequest {
    schema: String,
    generation_id: String,
    seed_ids: Vec<String>,
    relations: Vec<String>,
    direction: Option<String>,
    min_confidence: Option<u16>,
    max_results: Option<u16>,
    page_offset: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelationshipsResponse {
    schema: &'static str,
    context: QueryContextDto,
    groups: Vec<RelationshipGroupDto>,
    returned_edges: String,
    total_edges: String,
    exact: bool,
    truncated: bool,
    next_page_offset: Option<String>,
    completeness: CompletenessDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RelationshipGroupDto {
    seed_id: String,
    relation: String,
    direction: String,
    total_count: String,
    targets: Vec<RelationshipTargetDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RelationshipTargetDto {
    symbol_id: String,
    confidence: u16,
    source_references: Vec<SourceCapabilityDto>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceEncodingRequest {
    Utf8,
    BytesBase64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SourceRequest {
    schema: String,
    generation_id: String,
    source_capability: String,
    context_lines_before: Option<u8>,
    context_lines_after: Option<u8>,
    include_line_numbers: Option<bool>,
    encoding: Option<SourceEncodingRequest>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceResponse {
    schema: &'static str,
    repository_id: String,
    generation_id: String,
    chunks: Vec<SourceChunkDto>,
    total_source_bytes: String,
    truncated: bool,
    context: QueryContextDto,
    completeness: CompletenessDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceChunkDto {
    file_id: String,
    path: String,
    requested_start_byte: String,
    requested_end_byte: String,
    included_start_byte: String,
    included_end_byte: String,
    included_start_line: Option<String>,
    included_end_line: Option<String>,
    content: String,
    encoding: &'static str,
    content_hash: String,
    language: String,
    tier: &'static str,
    generated: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ChangeImpactRequest {
    schema: String,
    generation_id: String,
    changed_symbol_ids: Vec<String>,
    max_depth: Option<u8>,
    min_confidence: Option<u16>,
    include_tests: Option<bool>,
    max_dependents: Option<u16>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChangeImpactResponse {
    schema: &'static str,
    context: QueryContextDto,
    resolved_changes: Vec<ResolvedChangeDto>,
    impacted: Vec<ImpactGroupDto>,
    tests: Vec<ImpactTestDto>,
    risk_summary: RiskSummaryDto,
    completeness: CompletenessDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolvedChangeDto {
    symbol_id: Option<String>,
    file_id: Option<String>,
    classification: String,
    kind: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImpactGroupDto {
    source_index: u16,
    dependents: Vec<ImpactEntryDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImpactEntryDto {
    symbol_id: String,
    kind: String,
    distance: u8,
    confidence: u16,
    via: Vec<String>,
    is_public: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImpactTestDto {
    test_id: String,
    relevance: u16,
    why: Vec<String>,
    estimated_cost_ms: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RiskSummaryDto {
    level: String,
    reasons: Vec<String>,
    coverage: String,
    breaking_surface: bool,
    fanout: u32,
    dynamic_blind_spots: bool,
}

pub(crate) async fn node_detail(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((repository_id, node_id)): Path<(String, String)>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<NodeDetailResponse>, ApiError> {
    let repository = parse_repository(&repository_id, ApiError::invalid_node_request)?;
    let symbol = SymbolId::from_str(&node_id).map_err(|_| ApiError::invalid_node_request())?;
    let generation = parse_detail_query(raw_query.as_deref())?;
    let timeout = request_timeout(EVIDENCE_TIMEOUT)?;
    let explain = state
        .daemon()
        .symbol_explain(
            repository,
            GenerationSelector::Generation(generation),
            std::slice::from_ref(&symbol),
            timeout,
        )
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    validate_source_free_context(&explain.context, repository, generation)?;
    if explain.unresolved_symbols.contains(&symbol) && explain.symbols.is_empty() {
        return Err(ApiError::node_not_found());
    }
    let [explanation] = explain.symbols.as_slice() else {
        return Err(ApiError::daemon_response_invalid());
    };
    if explanation.symbol != symbol
        || !explain.unresolved_symbols.is_empty()
        || explanation.definition.repository() != repository
        || explanation.definition.generation() != generation
        || explanation.confidence > 1_000
        || !bounded_text(&explanation.kind, MAX_RESPONSE_LABEL_BYTES)
        || !bounded_text(&explanation.display_name, MAX_DISPLAY_NAME_BYTES)
        || explanation
            .signature
            .as_ref()
            .is_some_and(|value| !bounded_text(value, MAX_SIGNATURE_BYTES))
        || !bounded_text(&explanation.provider, MAX_RESPONSE_LABEL_BYTES)
        || !bounded_text(&explanation.evidence, MAX_RESPONSE_LABEL_BYTES)
        || !bounded_text(&explanation.language, MAX_RESPONSE_LABEL_BYTES)
    {
        return Err(ApiError::daemon_response_invalid());
    }
    let source = state
        .sources()
        .issue_many(
            session.identity(),
            std::slice::from_ref(&explanation.definition),
            Instant::now(),
        )
        .map_err(source_registry_error)?
        .into_iter()
        .map(map_source_capability)
        .collect();
    Ok(Json(NodeDetailResponse {
        schema: "rootlight.web-node-detail/1",
        repository_id: repository.to_string(),
        generation_id: generation.to_string(),
        node_id: symbol.to_string(),
        id_kind: "symbol",
        kind: explanation.kind.clone(),
        display_name: explanation.display_name.clone(),
        qualified_name: None,
        signature: explanation.signature.clone(),
        language: explanation.language.clone(),
        tier: analysis_tier_label(explanation.tier),
        confidence: explanation.confidence,
        provider: explanation.provider.clone(),
        evidence: explanation.evidence.clone(),
        outbound_exact: explanation.outbound_exact.to_string(),
        outbound_candidates: explanation.outbound_candidates.to_string(),
        inbound_exact: explanation.inbound_exact.to_string(),
        inbound_candidates: explanation.inbound_candidates.to_string(),
        reference_count: explanation.references_exact.to_string(),
        generated: None,
        source_references: source,
        context: map_context(&explain.context),
        completeness: map_completeness(&explain.execution_completeness),
    }))
}

pub(crate) async fn relationships(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(repository_id): Path<String>,
    Json(request): Json<RelationshipsRequest>,
) -> Result<Json<RelationshipsResponse>, ApiError> {
    let repository = parse_repository(&repository_id, ApiError::invalid_relationships_request)?;
    let parsed = parse_relationships_request(request)?;
    let timeout = request_timeout(EVIDENCE_TIMEOUT)?;
    let response = state
        .daemon()
        .symbol_relationships(
            repository,
            GenerationSelector::Generation(parsed.generation),
            &parsed.seeds,
            &parsed.relations,
            parsed.direction.as_deref(),
            parsed.min_confidence,
            Some(parsed.max_results),
            parsed.page_offset,
            timeout,
        )
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    validate_source_free_context(&response.context, repository, parsed.generation)?;
    let source_references = collect_relationship_sources(&response, &parsed)?;
    let issued = state
        .sources()
        .issue_many(session.identity(), &source_references, Instant::now())
        .map_err(source_registry_error)?;
    let mut capabilities = issued.into_iter();
    let groups = response
        .groups
        .iter()
        .map(|group| map_relationship_group(group, &mut capabilities))
        .collect::<Result<Vec<_>, _>>()?;
    if capabilities.next().is_some() {
        return Err(ApiError::daemon_response_invalid());
    }
    Ok(Json(RelationshipsResponse {
        schema: "rootlight.web-relationships/1",
        context: map_context(&response.context),
        groups,
        returned_edges: response.returned_edges.to_string(),
        total_edges: response.total_edges.to_string(),
        exact: response.exact,
        truncated: response.truncated,
        next_page_offset: response.next_page_offset.map(|value| value.to_string()),
        completeness: map_completeness(&response.execution_completeness),
    }))
}

pub(crate) async fn source(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(repository_id): Path<String>,
    Json(request): Json<SourceRequest>,
) -> Result<Response, ApiError> {
    let repository = parse_repository(&repository_id, ApiError::invalid_source_request)?;
    if request.schema != "rootlight.web-source-request/1"
        || request.source_capability.len() != 43
        || request
            .context_lines_before
            .is_some_and(|value| value > MAX_SOURCE_CONTEXT_LINES)
        || request
            .context_lines_after
            .is_some_and(|value| value > MAX_SOURCE_CONTEXT_LINES)
    {
        return Err(ApiError::invalid_source_request());
    }
    let generation = GenerationId::from_str(&request.generation_id)
        .map_err(|_| ApiError::invalid_source_request())?;
    let reference = state
        .sources()
        .take(
            session.identity(),
            &request.source_capability,
            repository,
            generation,
            Instant::now(),
        )
        .map_err(source_registry_error)?;
    let requested_encoding = request.encoding.unwrap_or(SourceEncodingRequest::Utf8);
    let projection = SourceReadOptions {
        context_lines_before: request.context_lines_before.unwrap_or(0),
        context_lines_after: request.context_lines_after.unwrap_or(0),
        merge_overlaps: false,
        include_line_numbers: request.include_line_numbers.unwrap_or(true),
        encoding: match requested_encoding {
            SourceEncodingRequest::Utf8 => SourceEncoding::Utf8,
            SourceEncodingRequest::BytesBase64 => SourceEncoding::Bytes,
        },
    };
    let timeout = request_timeout(SOURCE_TIMEOUT)?;
    let read = state
        .daemon()
        .source_read(
            repository,
            GenerationSelector::Generation(generation),
            std::slice::from_ref(&reference),
            projection,
            timeout,
        )
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    validate_source_read(&read, repository, generation, &reference)?;
    let dto = SourceResponse {
        schema: "rootlight.web-source/1",
        repository_id: repository.to_string(),
        generation_id: generation.to_string(),
        chunks: read
            .chunks
            .iter()
            .map(map_source_chunk)
            .collect::<Result<Vec<_>, _>>()?,
        total_source_bytes: read.total_source_bytes.to_string(),
        truncated: read.truncated,
        context: map_context(&read.context),
        completeness: map_completeness(&read.execution_completeness),
    };
    let mut response = Json(dto).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(PRAGMA, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

pub(crate) async fn change_impact(
    State(state): State<AppState>,
    Path(repository_id): Path<String>,
    Json(request): Json<ChangeImpactRequest>,
) -> Result<Json<ChangeImpactResponse>, ApiError> {
    let repository = parse_repository(&repository_id, ApiError::invalid_change_impact_request)?;
    let parsed = parse_change_impact_request(request)?;
    let timeout = request_timeout(EVIDENCE_TIMEOUT)?;
    let impact = state
        .daemon()
        .change_impact(
            repository,
            GenerationSelector::Generation(parsed.generation),
            &parsed.changed_symbols,
            parsed.max_depth,
            parsed.min_confidence,
            Some(parsed.include_tests),
            Some(parsed.max_dependents),
            timeout,
        )
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    validate_change_impact(
        &impact,
        repository,
        parsed.generation,
        parsed.max_dependents,
    )?;
    Ok(Json(map_change_impact(impact)))
}

struct ParsedRelationshipsRequest {
    generation: GenerationId,
    seeds: Vec<SymbolId>,
    relations: Vec<String>,
    direction: Option<String>,
    min_confidence: Option<u16>,
    max_results: u16,
    page_offset: u64,
}

fn parse_relationships_request(
    request: RelationshipsRequest,
) -> Result<ParsedRelationshipsRequest, ApiError> {
    if request.schema != "rootlight.web-relationships-request/1"
        || request.seed_ids.is_empty()
        || request.seed_ids.len() > MAX_RELATION_SEEDS
        || request.relations.is_empty()
        || request.relations.len() > MAX_RELATION_FAMILIES
        || request
            .min_confidence
            .is_some_and(|confidence| confidence > 1_000)
        || request
            .max_results
            .is_some_and(|maximum| !(1..=MAX_RELATION_RESULTS).contains(&maximum))
        || request
            .direction
            .as_deref()
            .is_some_and(|direction| !matches!(direction, "outbound" | "inbound" | "both"))
        || request
            .relations
            .iter()
            .any(|relation| !RELATION_ALLOWLIST.contains(&relation.as_str()))
        || has_duplicates(&request.seed_ids)
        || has_duplicates(&request.relations)
    {
        return Err(ApiError::invalid_relationships_request());
    }
    let generation = GenerationId::from_str(&request.generation_id)
        .map_err(|_| ApiError::invalid_relationships_request())?;
    let seeds = request
        .seed_ids
        .iter()
        .map(|value| {
            SymbolId::from_str(value).map_err(|_| ApiError::invalid_relationships_request())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let page_offset = request
        .page_offset
        .as_deref()
        .unwrap_or("0")
        .parse::<u64>()
        .ok()
        .filter(|value| *value <= MAX_RELATION_PAGE_OFFSET)
        .ok_or_else(ApiError::invalid_relationships_request)?;
    Ok(ParsedRelationshipsRequest {
        generation,
        seeds,
        relations: request.relations,
        direction: request.direction,
        min_confidence: request.min_confidence,
        max_results: request.max_results.unwrap_or(50),
        page_offset,
    })
}

fn collect_relationship_sources(
    response: &SymbolRelationships,
    request: &ParsedRelationshipsRequest,
) -> Result<Vec<rootlight_client::SourceReference>, ApiError> {
    let item_count = response.groups.iter().try_fold(0_usize, |total, group| {
        if !request.seeds.contains(&group.seed)
            || !request.relations.contains(&group.relation)
            || !matches!(group.direction.as_str(), "outbound" | "inbound")
            || !safe_label(&group.relation, MAX_RESPONSE_LABEL_BYTES)
            || group.items.iter().any(|target| target.confidence > 1_000)
        {
            return Err(ApiError::daemon_response_invalid());
        }
        total
            .checked_add(group.items.len())
            .ok_or_else(ApiError::daemon_response_invalid)
    })?;
    if item_count > usize::from(request.max_results)
        || response.returned_edges != u64::try_from(item_count).unwrap_or(u64::MAX)
    {
        return Err(ApiError::daemon_response_invalid());
    }
    let sources = response
        .groups
        .iter()
        .flat_map(|group| group.items.iter())
        .flat_map(|target| target.source_refs.iter().cloned())
        .collect::<Vec<_>>();
    if sources.len() > MAX_SOURCE_CAPABILITIES_PER_RESPONSE
        || sources.iter().any(|source| {
            source.repository() != response.context.repository
                || source.generation() != response.context.generation
        })
    {
        return Err(ApiError::daemon_response_invalid());
    }
    Ok(sources)
}

fn map_relationship_group(
    group: &RelationshipGroup,
    capabilities: &mut impl Iterator<Item = IssuedSourceCapability>,
) -> Result<RelationshipGroupDto, ApiError> {
    let targets = group
        .items
        .iter()
        .map(|target| map_relationship_target(target, capabilities))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RelationshipGroupDto {
        seed_id: group.seed.to_string(),
        relation: group.relation.clone(),
        direction: group.direction.clone(),
        total_count: group.total_count.to_string(),
        targets,
    })
}

fn map_relationship_target(
    target: &RelationshipTarget,
    capabilities: &mut impl Iterator<Item = IssuedSourceCapability>,
) -> Result<RelationshipTargetDto, ApiError> {
    if target.confidence > 1_000 {
        return Err(ApiError::daemon_response_invalid());
    }
    let source_references = (0..target.source_refs.len())
        .map(|_| {
            capabilities
                .next()
                .map(map_source_capability)
                .ok_or_else(ApiError::daemon_response_invalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RelationshipTargetDto {
        symbol_id: target.symbol.to_string(),
        confidence: target.confidence,
        source_references,
    })
}

struct ParsedChangeImpactRequest {
    generation: GenerationId,
    changed_symbols: Vec<SymbolId>,
    max_depth: Option<u8>,
    min_confidence: Option<u16>,
    include_tests: bool,
    max_dependents: u16,
}

fn parse_change_impact_request(
    request: ChangeImpactRequest,
) -> Result<ParsedChangeImpactRequest, ApiError> {
    if request.schema != "rootlight.web-change-impact-request/1"
        || request.changed_symbol_ids.is_empty()
        || request.changed_symbol_ids.len() > MAX_IMPACT_SEEDS
        || has_duplicates(&request.changed_symbol_ids)
        || request
            .max_depth
            .is_some_and(|depth| !(1..=8).contains(&depth))
        || request
            .min_confidence
            .is_some_and(|confidence| confidence > 1_000)
        || request
            .max_dependents
            .is_some_and(|maximum| !(1..=MAX_IMPACT_DEPENDENTS).contains(&maximum))
    {
        return Err(ApiError::invalid_change_impact_request());
    }
    let generation = GenerationId::from_str(&request.generation_id)
        .map_err(|_| ApiError::invalid_change_impact_request())?;
    let changed_symbols = request
        .changed_symbol_ids
        .iter()
        .map(|value| {
            SymbolId::from_str(value).map_err(|_| ApiError::invalid_change_impact_request())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ParsedChangeImpactRequest {
        generation,
        changed_symbols,
        max_depth: request.max_depth,
        min_confidence: request.min_confidence,
        include_tests: request.include_tests.unwrap_or(true),
        max_dependents: request.max_dependents.unwrap_or(100),
    })
}

fn validate_change_impact(
    impact: &ChangeImpact,
    repository: RepositoryId,
    generation: GenerationId,
    max_dependents: u16,
) -> Result<(), ApiError> {
    validate_source_free_context(&impact.context, repository, generation)?;
    let dependent_count = impact.impacted.iter().try_fold(0_usize, |total, group| {
        total
            .checked_add(group.dependents.len())
            .ok_or_else(ApiError::daemon_response_invalid)
    })?;
    if impact.resolved_changes.len() > MAX_IMPACT_SEEDS
        || impact.impacted.len() > MAX_IMPACT_SEEDS
        || dependent_count > usize::from(max_dependents)
        || impact.tests.len() > MAX_IMPACT_TESTS
        || impact.resolved_changes.iter().any(|change| {
            !safe_label(&change.classification, MAX_RESPONSE_LABEL_BYTES)
                || change
                    .kind
                    .as_ref()
                    .is_some_and(|kind| !safe_label(kind, MAX_RESPONSE_LABEL_BYTES))
        })
        || impact.impacted.iter().any(|group| {
            usize::from(group.source_index) >= impact.resolved_changes.len()
                || group.dependents.iter().any(|entry| {
                    entry.distance == 0
                        || entry.distance > 8
                        || entry.confidence > 1_000
                        || !safe_label(&entry.kind, MAX_RESPONSE_LABEL_BYTES)
                        || entry
                            .via
                            .iter()
                            .any(|label| !safe_label(label, MAX_RESPONSE_LABEL_BYTES))
                })
        })
        || impact.tests.iter().any(|test| {
            test.relevance > 1_000
                || !bounded_text(&test.test_id, MAX_RESPONSE_LABEL_BYTES)
                || test
                    .why
                    .iter()
                    .any(|reason| !safe_label(reason, MAX_RESPONSE_LABEL_BYTES))
        })
        || !safe_label(&impact.risk_summary.level, MAX_RESPONSE_LABEL_BYTES)
        || !safe_label(&impact.risk_summary.coverage, MAX_RESPONSE_LABEL_BYTES)
        || impact
            .risk_summary
            .reasons
            .iter()
            .any(|reason| !safe_label(reason, MAX_RESPONSE_LABEL_BYTES))
    {
        return Err(ApiError::daemon_response_invalid());
    }
    Ok(())
}

fn map_change_impact(impact: ChangeImpact) -> ChangeImpactResponse {
    ChangeImpactResponse {
        schema: "rootlight.web-change-impact/1",
        context: map_context(&impact.context),
        resolved_changes: impact
            .resolved_changes
            .into_iter()
            .map(|change| ResolvedChangeDto {
                symbol_id: change.symbol_id.map(|value| value.to_string()),
                file_id: change.file_id.map(|value| value.to_string()),
                classification: change.classification,
                kind: change.kind,
            })
            .collect(),
        impacted: impact
            .impacted
            .into_iter()
            .map(|group| ImpactGroupDto {
                source_index: group.source_index,
                dependents: group
                    .dependents
                    .into_iter()
                    .map(|entry| ImpactEntryDto {
                        symbol_id: entry.symbol_id.to_string(),
                        kind: entry.kind,
                        distance: entry.distance,
                        confidence: entry.confidence,
                        via: entry.via,
                        is_public: entry.is_public,
                    })
                    .collect(),
            })
            .collect(),
        tests: impact
            .tests
            .into_iter()
            .map(|test| ImpactTestDto {
                test_id: test.test_id,
                relevance: test.relevance,
                why: test.why,
                estimated_cost_ms: test.estimated_cost_ms,
            })
            .collect(),
        risk_summary: RiskSummaryDto {
            level: impact.risk_summary.level,
            reasons: impact.risk_summary.reasons,
            coverage: impact.risk_summary.coverage,
            breaking_surface: impact.risk_summary.breaking_surface,
            fanout: impact.risk_summary.fanout,
            dynamic_blind_spots: impact.risk_summary.dynamic_blind_spots,
        },
        completeness: map_completeness(&impact.execution_completeness),
    }
}

fn validate_source_read(
    read: &SourceRead,
    repository: RepositoryId,
    generation: GenerationId,
    requested: &rootlight_client::SourceReference,
) -> Result<(), ApiError> {
    validate_context(&read.context, repository, generation)?;
    let measured = read.chunks.iter().try_fold(0_u64, |total, chunk| {
        let bytes =
            u64::try_from(chunk.content.len()).map_err(|_| ApiError::daemon_response_invalid())?;
        total
            .checked_add(bytes)
            .ok_or_else(ApiError::daemon_response_invalid)
    })?;
    if read.chunks.len() > 1
        || read.total_source_bytes > MAX_SOURCE_BYTES
        || measured != read.total_source_bytes
        || read.context.usage.source_bytes != read.total_source_bytes
        || read.chunks.iter().any(|chunk| {
            chunk.source.repository() != repository
                || chunk.source.generation() != generation
                || &chunk.source != requested
                || chunk.content_hash != requested.content_hash()
                || chunk.start_byte > requested.byte_range().start
                || chunk.end_byte < requested.byte_range().end
                || chunk
                    .end_byte
                    .checked_sub(chunk.start_byte)
                    .and_then(|length| usize::try_from(length).ok())
                    != Some(chunk.content.len())
                || chunk.path.len() > MAX_SOURCE_PATH_BYTES
                || !bounded_text(&chunk.language, MAX_RESPONSE_LABEL_BYTES)
        })
    {
        return Err(ApiError::daemon_response_invalid());
    }
    Ok(())
}

fn map_source_chunk(chunk: &SourceChunk) -> Result<SourceChunkDto, ApiError> {
    let content = match chunk.encoding {
        SourceEncoding::Utf8 => String::from_utf8(chunk.content.clone())
            .map_err(|_| ApiError::daemon_response_invalid())?,
        SourceEncoding::Bytes => BASE64.encode(&chunk.content),
    };
    let requested = chunk.source.byte_range();
    Ok(SourceChunkDto {
        file_id: chunk.source.file().to_string(),
        path: chunk.path.clone(),
        requested_start_byte: requested.start.to_string(),
        requested_end_byte: requested.end.to_string(),
        included_start_byte: chunk.start_byte.to_string(),
        included_end_byte: chunk.end_byte.to_string(),
        included_start_line: chunk.start_line.map(|value| value.to_string()),
        included_end_line: chunk.end_line.map(|value| value.to_string()),
        content,
        encoding: match chunk.encoding {
            SourceEncoding::Utf8 => "utf8",
            SourceEncoding::Bytes => "base64",
        },
        content_hash: chunk.content_hash.to_string(),
        language: chunk.language.clone(),
        tier: analysis_tier_label(chunk.tier),
        generated: chunk.generated,
    })
}

fn parse_detail_query(raw_query: Option<&str>) -> Result<GenerationId, ApiError> {
    let Some(raw_query) = raw_query else {
        return Err(ApiError::invalid_node_request());
    };
    if raw_query.len() > MAX_DETAIL_QUERY_BYTES {
        return Err(ApiError::invalid_node_request());
    }
    let mut generation = None;
    let mut kind = None;
    let mut count = 0_usize;
    for (key, value) in form_urlencoded::parse(raw_query.as_bytes()) {
        count = count.saturating_add(1);
        if count > MAX_DETAIL_QUERY_PARAMETERS {
            return Err(ApiError::invalid_node_request());
        }
        match key.as_ref() {
            "generation" if generation.is_none() => {
                generation = Some(
                    GenerationId::from_str(value.as_ref())
                        .map_err(|_| ApiError::invalid_node_request())?,
                );
            }
            "kind" if kind.is_none() && value == "symbol" => kind = Some(()),
            _ => return Err(ApiError::invalid_node_request()),
        }
    }
    generation
        .filter(|_| kind.is_some())
        .ok_or_else(ApiError::invalid_node_request)
}

fn parse_repository(value: &str, error: fn() -> ApiError) -> Result<RepositoryId, ApiError> {
    RepositoryId::from_str(value).map_err(|_| error())
}

fn request_timeout(duration: Duration) -> Result<RequestTimeout, ApiError> {
    RequestTimeout::new(duration).map_err(|_| ApiError::daemon_unavailable())
}

fn validate_source_free_context(
    context: &QueryContext,
    repository: RepositoryId,
    generation: GenerationId,
) -> Result<(), ApiError> {
    validate_context(context, repository, generation)?;
    if context.usage.source_bytes != 0 {
        return Err(ApiError::daemon_response_invalid());
    }
    Ok(())
}

fn validate_context(
    context: &QueryContext,
    repository: RepositoryId,
    generation: GenerationId,
) -> Result<(), ApiError> {
    if context.repository != repository || context.generation != generation {
        return Err(ApiError::daemon_response_invalid());
    }
    Ok(())
}

fn map_source_capability(value: IssuedSourceCapability) -> SourceCapabilityDto {
    SourceCapabilityDto {
        capability: value.token,
        expires_in_seconds: value.expires_in_seconds,
    }
}

fn map_context(context: &QueryContext) -> QueryContextDto {
    QueryContextDto {
        repository_id: context.repository.to_string(),
        generation_id: context.generation.to_string(),
        parent_generation_id: context.parent_generation.map(|value| value.to_string()),
        active_generation: context.active_generation,
        structural_freshness: freshness_label(context.structural_freshness),
        semantic_freshness: freshness_label(context.semantic_freshness),
        tier: analysis_tier_label(context.tier),
        coverage_status: coverage_status_label(context.coverage_status),
        skipped_inputs: context.skipped_inputs.to_string(),
        usage: QueryUsageDto {
            rows: context.usage.rows.to_string(),
            edges: context.usage.edges.to_string(),
            results: context.usage.results.to_string(),
            source_bytes: context.usage.source_bytes.to_string(),
            json_bytes: context.usage.json_bytes.to_string(),
            estimated_tokens: context.usage.estimated_tokens.to_string(),
            token_accounting_profile: context.usage.token_accounting.map(token_accounting_label),
            memory_bytes: context.usage.memory_bytes.map(|value| value.to_string()),
            elapsed_micros: context.usage.elapsed_micros.to_string(),
        },
    }
}

fn map_completeness(completeness: &ResultCompleteness) -> CompletenessDto {
    CompletenessDto {
        state: completeness_state_label(completeness.state),
        limiting_resources: completeness
            .limiting_resources
            .iter()
            .map(|resource| LimitingResourceDto {
                kind: limiting_resource_label(resource.kind),
                limit: resource.limit.map(|value| value.to_string()),
                observed: resource.observed.map(|value| value.to_string()),
            })
            .collect(),
        continuation: continuation_label(completeness.continuation),
        guidance: completeness
            .guidance
            .iter()
            .copied()
            .map(guidance_label)
            .collect(),
    }
}

fn has_duplicates(values: &[String]) -> bool {
    values
        .iter()
        .enumerate()
        .any(|(index, value)| values[..index].contains(value))
}

fn bounded_text(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && !value.chars().any(|character| {
            character == '\0'
                || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        })
}

fn safe_label(value: &str, maximum_bytes: usize) -> bool {
    bounded_text(value, maximum_bytes)
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
}

fn source_registry_error(error: SourceRegistryError) -> ApiError {
    match error {
        SourceRegistryError::Invalid => ApiError::source_capability_invalid(),
        SourceRegistryError::LimitReached => ApiError::source_capability_limit_reached(),
        SourceRegistryError::ResourceUnavailable => ApiError::daemon_unavailable(),
    }
}

const fn analysis_tier_label(value: AnalysisTier) -> &'static str {
    match value {
        AnalysisTier::TierA => "tier_a",
        AnalysisTier::TierB => "tier_b",
        AnalysisTier::TierC => "tier_c",
        AnalysisTier::TierD => "tier_d",
    }
}

const fn coverage_status_label(value: CoverageStatus) -> &'static str {
    match value {
        CoverageStatus::Complete => "complete",
        CoverageStatus::Bounded => "bounded",
        CoverageStatus::Sampled => "sampled",
        CoverageStatus::Unknown => "unknown",
    }
}

const fn freshness_label(value: QueryFreshness) -> &'static str {
    match value {
        QueryFreshness::Current => "current",
        QueryFreshness::Stale => "stale",
        QueryFreshness::Superseded => "superseded",
    }
}

const fn completeness_state_label(value: ResultCompletenessState) -> &'static str {
    match value {
        ResultCompletenessState::Complete => "complete",
        ResultCompletenessState::Truncated => "truncated",
        ResultCompletenessState::UnsupportedPartial => "unsupported_partial",
        ResultCompletenessState::Indeterminate => "indeterminate",
    }
}

const fn continuation_label(value: ContinuationAvailability) -> &'static str {
    match value {
        ContinuationAvailability::NotApplicable => "not_applicable",
        ContinuationAvailability::Available => "available",
        ContinuationAvailability::Unavailable => "unavailable",
    }
}

const fn limiting_resource_label(value: LimitingResourceKind) -> &'static str {
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

const fn guidance_label(value: ContinuationGuidance) -> &'static str {
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

const fn token_accounting_label(value: TokenAccountingProfile) -> &'static str {
    match value {
        TokenAccountingProfile::Utf8ByteUpperBoundV1 => "utf8_byte_upper_bound_v1",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        fs,
        future::Future,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{
            Method, Request, StatusCode,
            header::{CONTENT_TYPE, COOKIE, HOST},
        },
    };
    use data_encoding::HEXLOWER;
    use rootlight_client::{
        ChangeImpactEntry, ChangeImpactGroup, ChangeImpactResolvedChange, ChangeImpactRiskSummary,
        ChangeImpactTest, ClientError, ContentHash, FileId, Health, LimitingResource, QueryUsage,
        RelationshipGroup, RelationshipTarget, RepositoryCatalogPage, RepositoryCatalogPageRequest,
        RepositoryStatus, RepositoryStatusRequest, SourceReference, SymbolExplain,
        SymbolExplanation,
    };
    use serde_json::{Value, json};
    use sha2::{Digest as _, Sha256};
    use tempfile::TempDir;
    use tower::ServiceExt as _;

    use crate::{
        app::{AppState, router},
        assets::AssetInventory,
        daemon::DaemonClient,
        filesystem_registry::FilesystemRegistry,
        graph_registry::GraphRegistry,
        index_registry::IndexRegistry,
        security::SecurityPolicy,
        session::{CSRF_HEADER_NAME, SESSION_COOKIE_NAME, SessionIdentity, SessionRegistry},
        support_registry::SupportRegistry,
    };

    const TEST_PORT: u16 = 43_127;
    const SOURCE_TEXT: &[u8] = b"let secret_source = 7;\n";
    const SOURCE_PATH: &str = "src/private.rs";

    struct TestHarness {
        router: Router,
        state: AppState,
        sessions: Arc<SessionRegistry>,
        session: TestSession,
    }

    struct TestSession {
        cookie: String,
        csrf: String,
        identity: SessionIdentity,
    }

    #[derive(Default)]
    struct EvidenceDaemon {
        fail: AtomicBool,
        wrong_generation: AtomicBool,
        evidence_calls: AtomicUsize,
        source_calls: AtomicUsize,
    }

    impl DaemonClient for EvidenceDaemon {
        fn health<'a>(
            &'a self,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<Health, ClientError>> + Send + 'a>> {
            Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
        }

        fn repository_catalog_page<'a>(
            &'a self,
            _request: &'a RepositoryCatalogPageRequest,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<RepositoryCatalogPage, ClientError>> + Send + 'a>>
        {
            Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
        }

        fn repository_status<'a>(
            &'a self,
            _request: RepositoryStatusRequest,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<RepositoryStatus, ClientError>> + Send + 'a>>
        {
            Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
        }

        fn symbol_explain<'a>(
            &'a self,
            repository: RepositoryId,
            generation: GenerationSelector,
            symbols: &'a [SymbolId],
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<SymbolExplain, ClientError>> + Send + 'a>> {
            self.evidence_calls.fetch_add(1, Ordering::Relaxed);
            let fail = self.fail.load(Ordering::Relaxed);
            let wrong_generation = self.wrong_generation.load(Ordering::Relaxed);
            Box::pin(async move {
                if fail {
                    return Err(ClientError::RequestTimedOut);
                }
                let GenerationSelector::Generation(generation) = generation else {
                    return Err(ClientError::InvalidFirstSliceRequest);
                };
                let symbol = symbols
                    .first()
                    .copied()
                    .ok_or(ClientError::InvalidFirstSliceRequest)?;
                let context = query_context(
                    repository,
                    if wrong_generation {
                        GenerationId::from_bytes([99; 20])
                    } else {
                        generation
                    },
                    1,
                    0,
                );
                if symbol == SymbolId::from_bytes([99; 20]) {
                    return Ok(SymbolExplain {
                        context,
                        symbols: Vec::new(),
                        unresolved_symbols: vec![symbol],
                        truncated: false,
                        execution_completeness: complete(),
                    });
                }
                Ok(SymbolExplain {
                    context,
                    symbols: vec![SymbolExplanation {
                        symbol,
                        kind: "function".to_owned(),
                        display_name: "<adversarial & name>".to_owned(),
                        signature: Some("fn evidence()".to_owned()),
                        definition: source_reference(repository, generation),
                        outbound_exact: u64::MAX,
                        outbound_candidates: 2,
                        inbound_exact: 3,
                        inbound_candidates: 4,
                        references_exact: u64::MAX,
                        provider: "treesitter".to_owned(),
                        evidence: "semantic_definition".to_owned(),
                        language: "rust".to_owned(),
                        tier: AnalysisTier::TierB,
                        confidence: 875,
                    }],
                    unresolved_symbols: Vec::new(),
                    truncated: false,
                    execution_completeness: complete(),
                })
            })
        }

        fn symbol_relationships<'a>(
            &'a self,
            repository: RepositoryId,
            generation: GenerationSelector,
            seeds: &'a [SymbolId],
            relations: &'a [String],
            direction: Option<&'a str>,
            _min_confidence: Option<u16>,
            _max_results: Option<u16>,
            _page_offset: u64,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<SymbolRelationships, ClientError>> + Send + 'a>>
        {
            self.evidence_calls.fetch_add(1, Ordering::Relaxed);
            let fail = self.fail.load(Ordering::Relaxed);
            Box::pin(async move {
                if fail {
                    return Err(ClientError::RequestTimedOut);
                }
                let GenerationSelector::Generation(generation) = generation else {
                    return Err(ClientError::InvalidFirstSliceRequest);
                };
                Ok(SymbolRelationships {
                    context: query_context(repository, generation, 1, 0),
                    groups: vec![RelationshipGroup {
                        seed: seeds[0],
                        relation: relations[0].clone(),
                        direction: direction.unwrap_or("outbound").to_owned(),
                        items: vec![RelationshipTarget {
                            symbol: SymbolId::from_bytes([8; 20]),
                            confidence: 700,
                            source_refs: vec![source_reference(repository, generation)],
                        }],
                        total_count: u64::MAX,
                    }],
                    returned_edges: 1,
                    total_edges: u64::MAX,
                    exact: false,
                    truncated: true,
                    next_page_offset: Some(1),
                    execution_completeness: truncated(),
                })
            })
        }

        fn source_read<'a>(
            &'a self,
            repository: RepositoryId,
            generation: GenerationSelector,
            references: &'a [SourceReference],
            projection: SourceReadOptions,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<SourceRead, ClientError>> + Send + 'a>> {
            self.source_calls.fetch_add(1, Ordering::Relaxed);
            let fail = self.fail.load(Ordering::Relaxed);
            Box::pin(async move {
                if fail {
                    return Err(ClientError::RequestTimedOut);
                }
                let GenerationSelector::Generation(generation) = generation else {
                    return Err(ClientError::InvalidFirstSliceRequest);
                };
                let reference = references
                    .first()
                    .cloned()
                    .ok_or(ClientError::InvalidFirstSliceRequest)?;
                let source_bytes = u64::try_from(SOURCE_TEXT.len())
                    .map_err(|_| ClientError::InvalidResponseCorrelation)?;
                Ok(SourceRead {
                    context: query_context(repository, generation, 1, source_bytes),
                    chunks: vec![SourceChunk {
                        source: reference.clone(),
                        path: SOURCE_PATH.to_owned(),
                        start_byte: reference.byte_range().start,
                        end_byte: reference.byte_range().end,
                        start_line: projection.include_line_numbers.then_some(1),
                        end_line: projection.include_line_numbers.then_some(1),
                        content: SOURCE_TEXT.to_vec(),
                        encoding: projection.encoding,
                        content_hash: reference.content_hash(),
                        language: "rust".to_owned(),
                        tier: AnalysisTier::TierB,
                        generated: false,
                    }],
                    total_source_bytes: source_bytes,
                    truncated: false,
                    execution_completeness: complete(),
                })
            })
        }

        fn change_impact<'a>(
            &'a self,
            repository: RepositoryId,
            generation: GenerationSelector,
            changed_symbols: &'a [SymbolId],
            _max_depth: Option<u8>,
            _min_confidence: Option<u16>,
            include_tests: Option<bool>,
            _max_dependents: Option<u16>,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<ChangeImpact, ClientError>> + Send + 'a>> {
            self.evidence_calls.fetch_add(1, Ordering::Relaxed);
            let fail = self.fail.load(Ordering::Relaxed);
            Box::pin(async move {
                if fail {
                    return Err(ClientError::RequestTimedOut);
                }
                let GenerationSelector::Generation(generation) = generation else {
                    return Err(ClientError::InvalidFirstSliceRequest);
                };
                Ok(ChangeImpact {
                    context: query_context(repository, generation, 1, 0),
                    resolved_changes: vec![ChangeImpactResolvedChange {
                        symbol_id: Some(changed_symbols[0]),
                        file_id: Some(FileId::from_bytes([4; 20])),
                        classification: "surface".to_owned(),
                        kind: Some("function".to_owned()),
                    }],
                    impacted: vec![ChangeImpactGroup {
                        source_index: 0,
                        dependents: vec![ChangeImpactEntry {
                            symbol_id: SymbolId::from_bytes([8; 20]),
                            kind: "function".to_owned(),
                            distance: 1,
                            confidence: 800,
                            via: vec!["calls".to_owned()],
                            is_public: true,
                        }],
                    }],
                    tests: include_tests
                        .unwrap_or(false)
                        .then(|| ChangeImpactTest {
                            test_id: "sym1_test_candidate".to_owned(),
                            relevance: 750,
                            why: vec!["direct_dependency".to_owned()],
                            estimated_cost_ms: Some(12),
                        })
                        .into_iter()
                        .collect(),
                    risk_summary: ChangeImpactRiskSummary {
                        level: "high".to_owned(),
                        reasons: vec!["public_surface".to_owned()],
                        coverage: "bounded".to_owned(),
                        breaking_surface: true,
                        fanout: 1,
                        dynamic_blind_spots: false,
                    },
                    execution_completeness: truncated(),
                })
            })
        }
    }

    #[tokio::test]
    async fn authentication_and_csrf_reject_before_parsing_or_daemon_work() {
        let daemon = Arc::new(EvidenceDaemon::default());
        let harness = harness(Arc::clone(&daemon));
        let repository = repository();

        let unauthenticated_detail = Request::builder()
            .uri("/api/v1/projects/not-an-id/nodes/not-an-id?generation=active&kind=file")
            .header(HOST, authority())
            .header("sec-fetch-site", "same-origin")
            .body(Body::empty())
            .expect("detail request builds");
        assert_eq!(
            harness
                .router
                .clone()
                .oneshot(unauthenticated_detail)
                .await
                .expect("detail rejection returns")
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let unauthenticated_source = post_request(
            &format!("/api/v1/projects/{repository}/source"),
            None,
            None,
            "{",
        );
        assert_eq!(
            harness
                .router
                .clone()
                .oneshot(unauthenticated_source)
                .await
                .expect("source auth rejection returns")
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let missing_csrf = post_request(
            &format!("/api/v1/projects/{repository}/source"),
            Some(&harness.session.cookie),
            None,
            "{",
        );
        assert_eq!(
            harness
                .router
                .oneshot(missing_csrf)
                .await
                .expect("source CSRF rejection returns")
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(daemon.evidence_calls.load(Ordering::Relaxed), 0);
        assert_eq!(daemon.source_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn node_detail_is_exact_source_free_and_unknown_symbols_are_not_found() {
        let daemon = Arc::new(EvidenceDaemon::default());
        let harness = harness(Arc::clone(&daemon));
        let response = harness
            .router
            .clone()
            .oneshot(detail_request(
                repository(),
                generation(),
                symbol(),
                &harness.session.cookie,
            ))
            .await
            .expect("node detail returns");
        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;
        assert_eq!(json["schema"], "rootlight.web-node-detail/1");
        assert_eq!(json["generationId"], generation().to_string());
        assert_eq!(json["confidence"], 875);
        assert_eq!(json["outboundExact"], u64::MAX.to_string());
        assert_eq!(json["context"]["usage"]["sourceBytes"], "0");
        assert_eq!(json["sourceReferences"].as_array().map(Vec::len), Some(1));
        let serialized = serde_json::to_string(&json).expect("detail serializes");
        assert!(!serialized.contains(SOURCE_PATH));
        assert!(!serialized.contains("secret_source"));
        assert!(!serialized.contains("contentHash"));
        assert!(!serialized.contains("requestedStartByte"));

        let active_generation = Request::builder()
            .uri(format!(
                "/api/v1/projects/{}/nodes/{}?generation=active&kind=symbol",
                repository(),
                symbol()
            ))
            .header(HOST, authority())
            .header("sec-fetch-site", "same-origin")
            .header(COOKIE, &harness.session.cookie)
            .body(Body::empty())
            .expect("active-generation detail request builds");
        assert_eq!(
            harness
                .router
                .clone()
                .oneshot(active_generation)
                .await
                .expect("active-generation rejection returns")
                .status(),
            StatusCode::BAD_REQUEST
        );

        let unknown = harness
            .router
            .oneshot(detail_request(
                repository(),
                generation(),
                SymbolId::from_bytes([99; 20]),
                &harness.session.cookie,
            ))
            .await
            .expect("unknown node response returns");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn source_capability_is_cross_session_safe_single_use_and_no_store() {
        let daemon = Arc::new(EvidenceDaemon::default());
        let harness = harness(Arc::clone(&daemon));
        let detail = harness
            .router
            .clone()
            .oneshot(detail_request(
                repository(),
                generation(),
                symbol(),
                &harness.session.cookie,
            ))
            .await
            .expect("detail response returns");
        let detail_json = response_json(detail).await;
        let capability = detail_json["sourceReferences"][0]["capability"]
            .as_str()
            .expect("source capability returns")
            .to_owned();
        let other = issue_session(&harness.sessions);
        let source_body = source_request_body(generation(), &capability);

        let cross_session = post_request(
            &format!("/api/v1/projects/{}/source", repository()),
            Some(&other.cookie),
            Some(&other.csrf),
            &source_body,
        );
        assert_eq!(
            harness
                .router
                .clone()
                .oneshot(cross_session)
                .await
                .expect("cross-session response returns")
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(daemon.source_calls.load(Ordering::Relaxed), 0);

        let source_request = post_request(
            &format!("/api/v1/projects/{}/source", repository()),
            Some(&harness.session.cookie),
            Some(&harness.session.csrf),
            &source_body,
        );
        let source_response = harness
            .router
            .clone()
            .oneshot(source_request)
            .await
            .expect("source response returns");
        assert_eq!(source_response.status(), StatusCode::OK);
        assert_eq!(
            source_response.headers().get(CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-store"))
        );
        assert_eq!(
            source_response.headers().get(PRAGMA),
            Some(&HeaderValue::from_static("no-cache"))
        );
        assert_eq!(
            source_response.headers().get(X_CONTENT_TYPE_OPTIONS),
            Some(&HeaderValue::from_static("nosniff"))
        );
        let source_json = response_json(source_response).await;
        assert_eq!(source_json["chunks"][0]["path"], SOURCE_PATH);
        assert_eq!(
            source_json["chunks"][0]["content"],
            String::from_utf8_lossy(SOURCE_TEXT).as_ref()
        );
        assert_eq!(
            source_json["totalSourceBytes"],
            SOURCE_TEXT.len().to_string()
        );

        let replay = post_request(
            &format!("/api/v1/projects/{}/source", repository()),
            Some(&harness.session.cookie),
            Some(&harness.session.csrf),
            &source_body,
        );
        assert_eq!(
            harness
                .router
                .clone()
                .oneshot(replay)
                .await
                .expect("source replay rejection returns")
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(daemon.source_calls.load(Ordering::Relaxed), 1);

        let detail = harness
            .router
            .clone()
            .oneshot(detail_request(
                repository(),
                generation(),
                symbol(),
                &harness.session.cookie,
            ))
            .await
            .expect("replacement detail response returns");
        let detail_json = response_json(detail).await;
        let capability = detail_json["sourceReferences"][0]["capability"]
            .as_str()
            .expect("replacement source capability returns");
        let bytes_body = serde_json::to_string(&json!({
            "schema": "rootlight.web-source-request/1",
            "generationId": generation(),
            "sourceCapability": capability,
            "encoding": "bytes_base64"
        }))
        .expect("binary source request serializes");
        let binary = harness
            .router
            .oneshot(post_request(
                &format!("/api/v1/projects/{}/source", repository()),
                Some(&harness.session.cookie),
                Some(&harness.session.csrf),
                &bytes_body,
            ))
            .await
            .expect("binary source response returns");
        assert_eq!(binary.status(), StatusCode::OK);
        let binary_json = response_json(binary).await;
        assert_eq!(binary_json["chunks"][0]["encoding"], "base64");
        assert_eq!(
            binary_json["chunks"][0]["content"],
            BASE64.encode(SOURCE_TEXT)
        );
        assert_eq!(daemon.source_calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn relationship_groups_and_change_impact_preserve_typed_semantics() {
        let daemon = Arc::new(EvidenceDaemon::default());
        let harness = harness(daemon);
        let relationships_body = serde_json::to_string(&json!({
            "schema": "rootlight.web-relationships-request/1",
            "generationId": generation(),
            "seedIds": [symbol()],
            "relations": ["calls"],
            "direction": "outbound",
            "minConfidence": 250,
            "maxResults": 10,
            "pageOffset": "0"
        }))
        .expect("relationships request serializes");
        let relationships = harness
            .router
            .clone()
            .oneshot(post_request(
                &format!("/api/v1/projects/{}/relationships", repository()),
                Some(&harness.session.cookie),
                Some(&harness.session.csrf),
                &relationships_body,
            ))
            .await
            .expect("relationships response returns");
        assert_eq!(relationships.status(), StatusCode::OK);
        let relationships_json = response_json(relationships).await;
        assert_eq!(relationships_json["groups"][0]["relation"], "calls");
        assert_eq!(relationships_json["groups"][0]["direction"], "outbound");
        assert_eq!(
            relationships_json["groups"][0]["totalCount"],
            u64::MAX.to_string()
        );
        assert_eq!(
            relationships_json["groups"][0]["targets"][0]["confidence"],
            700
        );
        assert_eq!(relationships_json["completeness"]["state"], "truncated");
        assert_eq!(
            relationships_json["completeness"]["guidance"][0],
            "use_cursor"
        );
        let serialized =
            serde_json::to_string(&relationships_json).expect("relationships serialize");
        assert!(!serialized.contains(SOURCE_PATH));
        assert!(!serialized.contains("secret_source"));

        let impact_body = serde_json::to_string(&json!({
            "schema": "rootlight.web-change-impact-request/1",
            "generationId": generation(),
            "changedSymbolIds": [symbol()],
            "maxDepth": 4,
            "minConfidence": 250,
            "includeTests": true,
            "maxDependents": 20
        }))
        .expect("impact request serializes");
        let impact = harness
            .router
            .oneshot(post_request(
                &format!("/api/v1/projects/{}/change-impact", repository()),
                Some(&harness.session.cookie),
                Some(&harness.session.csrf),
                &impact_body,
            ))
            .await
            .expect("impact response returns");
        assert_eq!(impact.status(), StatusCode::OK);
        let impact_json = response_json(impact).await;
        assert_eq!(
            impact_json["resolvedChanges"][0]["classification"],
            "surface"
        );
        assert_eq!(
            impact_json["impacted"][0]["dependents"][0]["via"][0],
            "calls"
        );
        assert_eq!(impact_json["riskSummary"]["level"], "high");
        assert_eq!(impact_json["tests"][0]["why"][0], "direct_dependency");
        assert_eq!(impact_json["completeness"]["state"], "truncated");
        let serialized = serde_json::to_string(&impact_json).expect("impact serializes");
        assert!(!serialized.contains(SOURCE_PATH));
        assert!(!serialized.contains("secret_source"));
    }

    #[tokio::test]
    async fn requests_and_daemon_responses_fail_closed_on_bounds_and_correlation() {
        let daemon = Arc::new(EvidenceDaemon::default());
        let harness = harness(Arc::clone(&daemon));
        let oversized_seeds = (1_u8..=9)
            .map(|value| SymbolId::from_bytes([value; 20]).to_string())
            .collect::<Vec<_>>();
        let relationships_body = serde_json::to_string(&json!({
            "schema": "rootlight.web-relationships-request/1",
            "generationId": generation(),
            "seedIds": oversized_seeds,
            "relations": ["calls"]
        }))
        .expect("bounded request serializes");
        let bounded = harness
            .router
            .clone()
            .oneshot(post_request(
                &format!("/api/v1/projects/{}/relationships", repository()),
                Some(&harness.session.cookie),
                Some(&harness.session.csrf),
                &relationships_body,
            ))
            .await
            .expect("bounded rejection returns");
        assert_eq!(bounded.status(), StatusCode::BAD_REQUEST);
        assert_eq!(daemon.evidence_calls.load(Ordering::Relaxed), 0);

        let invalid_source_body = serde_json::to_string(&json!({
            "schema": "rootlight.web-source-request/1",
            "generationId": generation(),
            "sourceCapability": "x".repeat(43),
            "contextLinesBefore": 9
        }))
        .expect("source request serializes");
        let invalid_source = harness
            .router
            .clone()
            .oneshot(post_request(
                &format!("/api/v1/projects/{}/source", repository()),
                Some(&harness.session.cookie),
                Some(&harness.session.csrf),
                &invalid_source_body,
            ))
            .await
            .expect("source bound rejection returns");
        assert_eq!(invalid_source.status(), StatusCode::BAD_REQUEST);
        assert_eq!(daemon.source_calls.load(Ordering::Relaxed), 0);

        daemon.wrong_generation.store(true, Ordering::Relaxed);
        let mismatched = harness
            .router
            .clone()
            .oneshot(detail_request(
                repository(),
                generation(),
                symbol(),
                &harness.session.cookie,
            ))
            .await
            .expect("correlation rejection returns");
        assert_eq!(mismatched.status(), StatusCode::BAD_GATEWAY);
        let mismatch_json = response_json(mismatched).await;
        assert_eq!(mismatch_json["error"]["code"], "daemon_response_invalid");

        daemon.wrong_generation.store(false, Ordering::Relaxed);
        daemon.fail.store(true, Ordering::Relaxed);
        let unavailable = harness
            .router
            .oneshot(detail_request(
                repository(),
                generation(),
                symbol(),
                &harness.session.cookie,
            ))
            .await
            .expect("daemon error returns");
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
        let unavailable_json = response_json(unavailable).await;
        let serialized = serde_json::to_string(&unavailable_json).expect("error serializes");
        assert_eq!(unavailable_json["error"]["code"], "daemon_unavailable");
        assert!(!serialized.contains(SOURCE_PATH));
        assert!(!serialized.contains("secret_source"));
    }

    #[tokio::test]
    async fn logout_clears_issued_source_capabilities() {
        let daemon = Arc::new(EvidenceDaemon::default());
        let harness = harness(daemon);
        let detail = harness
            .router
            .clone()
            .oneshot(detail_request(
                repository(),
                generation(),
                symbol(),
                &harness.session.cookie,
            ))
            .await
            .expect("detail response returns");
        let detail_json = response_json(detail).await;
        let capability = detail_json["sourceReferences"][0]["capability"]
            .as_str()
            .expect("source capability returns")
            .to_owned();

        let logout = Request::builder()
            .method(Method::DELETE)
            .uri("/api/v1/session")
            .header(HOST, authority())
            .header("origin", origin())
            .header("sec-fetch-site", "same-origin")
            .header(COOKIE, &harness.session.cookie)
            .header(CSRF_HEADER_NAME, &harness.session.csrf)
            .body(Body::empty())
            .expect("logout request builds");
        assert_eq!(
            harness
                .router
                .oneshot(logout)
                .await
                .expect("logout response returns")
                .status(),
            StatusCode::NO_CONTENT
        );
        assert!(
            harness
                .state
                .sources()
                .take(
                    harness.session.identity,
                    &capability,
                    repository(),
                    generation(),
                    Instant::now(),
                )
                .is_err()
        );
    }

    fn harness(daemon: Arc<EvidenceDaemon>) -> TestHarness {
        let assets = test_assets();
        let sessions = Arc::new(SessionRegistry::new());
        let session = issue_session(&sessions);
        let state = AppState::new(
            assets,
            daemon,
            Arc::clone(&sessions),
            Arc::new(FilesystemRegistry::new()),
            Arc::new(IndexRegistry::new()),
            Arc::new(GraphRegistry::new()),
            Arc::new(SupportRegistry::new()),
        );
        TestHarness {
            router: router(state.clone(), SecurityPolicy::loopback(TEST_PORT)),
            state,
            sessions,
            session,
        }
    }

    fn test_assets() -> AssetInventory {
        let root = TempDir::new().expect("asset root exists");
        let index = b"<!doctype html><html class=\"dark\"></html>";
        fs::write(root.path().join("index.html"), index).expect("index writes");
        let manifest = serde_json::to_vec(&json!({
            "schema_version": 1,
            "assets": [{
                "path": "index.html",
                "bytes": index.len(),
                "sha256": HEXLOWER.encode(Sha256::digest(index).as_ref())
            }]
        }))
        .expect("manifest serializes");
        fs::write(root.path().join("asset-manifest.json"), manifest).expect("manifest writes");
        AssetInventory::load(root.path()).expect("assets validate")
    }

    fn issue_session(sessions: &SessionRegistry) -> TestSession {
        let now = Instant::now();
        let bootstrap = sessions
            .issue_bootstrap(now)
            .expect("bootstrap issues")
            .encoded()
            .to_owned();
        let credentials = sessions
            .consume_bootstrap(&bootstrap, now)
            .expect("session issues");
        let identity = sessions
            .authenticate(&credentials.cookie_value, now)
            .expect("session authenticates")
            .identity();
        TestSession {
            cookie: format!("{SESSION_COOKIE_NAME}={}", credentials.cookie_value),
            csrf: credentials.csrf_token,
            identity,
        }
    }

    fn detail_request(
        repository: RepositoryId,
        generation: GenerationId,
        symbol: SymbolId,
        cookie: &str,
    ) -> Request<Body> {
        Request::builder()
            .uri(format!(
                "/api/v1/projects/{repository}/nodes/{symbol}?generation={generation}&kind=symbol"
            ))
            .header(HOST, authority())
            .header("sec-fetch-site", "same-origin")
            .header(COOKIE, cookie)
            .body(Body::empty())
            .expect("detail request builds")
    }

    fn post_request(
        uri: &str,
        cookie: Option<&str>,
        csrf: Option<&str>,
        body: &str,
    ) -> Request<Body> {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(HOST, authority())
            .header("origin", origin())
            .header("sec-fetch-site", "same-origin")
            .header(CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie);
        }
        if let Some(csrf) = csrf {
            request = request.header(CSRF_HEADER_NAME, csrf);
        }
        request
            .body(Body::from(body.to_owned()))
            .expect("POST request builds")
    }

    fn source_request_body(generation: GenerationId, capability: &str) -> String {
        serde_json::to_string(&json!({
            "schema": "rootlight.web-source-request/1",
            "generationId": generation,
            "sourceCapability": capability,
            "contextLinesBefore": 0,
            "contextLinesAfter": 0,
            "includeLineNumbers": true,
            "encoding": "utf8"
        }))
        .expect("source request serializes")
    }

    async fn response_json(response: Response) -> Value {
        let body = to_bytes(response.into_body(), 256 * 1024)
            .await
            .expect("response body reads");
        serde_json::from_slice(&body).expect("response JSON parses")
    }

    fn query_context(
        repository: RepositoryId,
        generation: GenerationId,
        results: u64,
        source_bytes: u64,
    ) -> QueryContext {
        QueryContext {
            repository,
            generation,
            parent_generation: None,
            active_generation: true,
            structural_freshness: QueryFreshness::Current,
            semantic_freshness: QueryFreshness::Stale,
            tier: AnalysisTier::TierB,
            coverage_status: CoverageStatus::Bounded,
            skipped_inputs: 2,
            usage: QueryUsage {
                rows: 10,
                edges: 5,
                results,
                source_bytes,
                json_bytes: 512,
                estimated_tokens: 128,
                token_accounting: None,
                memory_bytes: Some(1_024),
                elapsed_micros: 50,
            },
        }
    }

    fn complete() -> ResultCompleteness {
        ResultCompleteness {
            state: ResultCompletenessState::Complete,
            limiting_resources: Vec::new(),
            continuation: ContinuationAvailability::NotApplicable,
            guidance: Vec::new(),
        }
    }

    fn truncated() -> ResultCompleteness {
        ResultCompleteness {
            state: ResultCompletenessState::Truncated,
            limiting_resources: vec![LimitingResource {
                kind: LimitingResourceKind::Results,
                limit: Some(1),
                observed: Some(2),
            }],
            continuation: ContinuationAvailability::Available,
            guidance: vec![ContinuationGuidance::UseCursor],
        }
    }

    fn source_reference(repository: RepositoryId, generation: GenerationId) -> SourceReference {
        SourceReference::new(
            repository,
            generation,
            FileId::from_bytes([4; 20]),
            0..u64::try_from(SOURCE_TEXT.len()).expect("fixture length fits u64"),
            ContentHash::from_bytes([5; 32]),
            Some(1..=1),
        )
        .expect("source fixture is valid")
    }

    fn repository() -> RepositoryId {
        RepositoryId::from_bytes([1; 16])
    }

    fn generation() -> GenerationId {
        GenerationId::from_bytes([2; 20])
    }

    fn symbol() -> SymbolId {
        SymbolId::from_bytes([3; 20])
    }

    fn authority() -> String {
        format!("127.0.0.1:{TEST_PORT}")
    }

    fn origin() -> String {
        format!("http://{}", authority())
    }
}
