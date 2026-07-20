use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io, mem,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{FactId, FileId, GenerationId, SymbolId};
use rootlight_ir::{
    AnalysisTier, CoverageRecord, CoverageScope, EntityFlag, EntityKind, EntityVisibility,
    NormalizedIrDocument, OccurrenceTarget, RelationEndpoint, RelationPredicate, SourceRef,
};
use rootlight_search::{LexicalSearch, SearchBudget, SearchRequest, validate_search_request};
use rootlight_source::{SourceBudget, SourceError, SourceReadOptions, SourceService};
use rootlight_storage::GenerationSnapshot;
use serde::Serialize;

use crate::model::{
    ArchitectureCyclesPlan, ArchitectureCyclesProjection, ArchitectureCyclesResult,
    CodeDeadBlindSpot, CodeDeadEntryPointPolicy, CodeDeadEntryPointSummary, CodeDeadPlan,
    CodeDeadResult, CodeDeadSuppressionRule, CodeLocatePlan, CodeLocateResult, CycleBreak,
    CycleComponent, CyclePath, DeadCodeCandidate, DeadCodeClassification, FlowTraceEdge,
    FlowTraceFrontier, FlowTracePath, FlowTracePlan, FlowTraceProjection, FlowTraceResult,
    LocateHit, LocateMode, PlanEstimate, PlanExplanation, PlanKind, QueryBudget, QueryError,
    QueryOperator, QueryResource, QueryResponse, QueryUsage, RelationDirection, RelationFamily,
    RelationshipEdgeTarget, RelationshipGroup, RepositoryDataTrust, SourceChunkResult,
    SourceReadPlan, SourceReadQueryResult, SymbolExplainPlan, SymbolExplainResult,
    SymbolRelationshipsPlan, SymbolRelationshipsResult, TokenAccountingProfile, checked_add,
    checked_u128_to_u64, checked_usize_to_u64, ensure_estimate, search_mode,
};

/// Daemon-independent typed query service pinned to normalized IR and lexical data.
pub struct QueryService<'generation, Search> {
    generation: &'generation GenerationSnapshot,
    search: &'generation Search,
}

impl<'generation, Search> QueryService<'generation, Search>
where
    Search: LexicalSearch,
{
    /// Binds normalized and lexical readers only when their generation agrees.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationMismatch`] when the lexical index serves
    /// another immutable generation.
    pub fn new(
        generation: &'generation GenerationSnapshot,
        search: &'generation Search,
    ) -> Result<Self, QueryError> {
        if generation.metadata().generation() != search.generation() {
            return Err(QueryError::GenerationMismatch);
        }
        Ok(Self { generation, search })
    }

    /// Builds a deterministic bounded `code.locate` plan.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, result limit, arithmetic
    /// overflow, or a conservative estimate that cannot be admitted.
    pub fn plan_code_locate(
        &self,
        query: String,
        mode: LocateMode,
        max_results: usize,
        mut search_budget: SearchBudget,
        budget: QueryBudget,
    ) -> Result<CodeLocatePlan, QueryError> {
        budget.validate()?;
        if max_results == 0
            || max_results > search_budget.max_results
            || checked_usize_to_u64(max_results)? > budget.max_results
        {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        search_budget.max_duration = search_budget.max_duration.min(budget.max_duration);
        let request = SearchRequest {
            query,
            mode: search_mode(mode),
            max_results,
        };
        validate_search_request(&request, search_budget)?;
        let mandatory_rows = checked_add(
            checked_usize_to_u64(search_budget.max_candidates)?,
            checked_usize_to_u64(max_results)?,
            QueryResource::Rows,
            u64::MAX,
        )?;
        if mandatory_rows > budget.max_rows {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Rows,
            });
        }
        let estimate = PlanEstimate {
            rows: budget.max_rows,
            edges: 0,
            results: budget.max_results,
            source_bytes: 0,
            // Repository metadata is bounded when the generation is admitted,
            // but its exact matching subset is unknown until search executes.
            memory_bytes: budget.max_memory_bytes,
            json_bytes: budget.max_json_bytes,
            estimated_tokens: budget.max_tokens,
            duration_micros: duration_micros(budget.max_duration),
        };
        ensure_estimate(estimate, budget)?;
        let explanation = PlanExplanation {
            generation: self.generation.metadata().generation(),
            kind: PlanKind::CodeLocate,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::LexicalSearch,
                QueryOperator::EntityHydration,
                QueryOperator::CoverageProjection,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(CodeLocatePlan {
            query: request.query,
            mode,
            max_results,
            search_budget,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `code.locate` plan.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, lexical failure, generation
    /// drift, normalized-data drift, output encoding, or resource exhaustion.
    pub fn execute_code_locate(
        &self,
        plan: &CodeLocatePlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<CodeLocateResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let request = SearchRequest {
            query: plan.query.clone(),
            mode: search_mode(plan.mode),
            max_results: plan.max_results,
        };
        let outcome = self
            .search
            .search_with_stats(&request, plan.search_budget, cancellation)?;
        control.check()?;
        if outcome.hits.len() > plan.max_results
            || outcome.matched_candidates < checked_usize_to_u64(outcome.hits.len())?
            || outcome.matched_candidates > checked_usize_to_u64(plan.search_budget.max_candidates)?
            || outcome.materialized_text_bytes
                > checked_usize_to_u64(plan.search_budget.max_returned_text_bytes)?
            || outcome.materialized_text_bytes < search_hit_text_bytes(&outcome.hits)?
        {
            return Err(QueryError::IndexDrift);
        }

        let matched_candidates = outcome.matched_candidates;
        let mut tracker = UsageTracker::new(plan.budget);
        tracker.add_rows(outcome.matched_candidates)?;
        let mut limiting_resources = Vec::new();
        if matched_candidates > checked_usize_to_u64(outcome.hits.len())? {
            record_limit(&mut limiting_resources, QueryResource::Results)?;
        }
        let mut located = Vec::new();
        try_reserve(&mut located, outcome.hits.len())?;
        let mut symbols = BTreeSet::new();
        let mut files = BTreeSet::new();
        for hit in outcome.hits {
            control.check()?;
            if !hit.relevance_score.is_finite() {
                return Err(QueryError::IndexDrift);
            }
            let entity = find_entity(self.generation.document(), hit.symbol_id)
                .ok_or(QueryError::IndexDrift)?;
            let file =
                find_file(self.generation.document(), hit.file_id).ok_or(QueryError::IndexDrift)?;
            let source = entity
                .evidence
                .source
                .as_ref()
                .ok_or(QueryError::IndexDrift)?;
            if entity.qualified_name != hit.qualified_name
                || entity.display_name != hit.identifier
                || entity.language != hit.language
                || file.path != hit.path
                || file.generated != hit.generated
                || source.repository() != self.generation.metadata().repository()
                || source.generation() != self.generation.metadata().generation()
                || source.span().file() != hit.file_id
                || source.content_hash() != file.content_hash
                || serialized_label(&entity.kind)? != hit.kind
                || serialized_label(&entity.tier)? != hit.tier
            {
                return Err(QueryError::IndexDrift);
            }
            tracker.add_rows(1)?;
            tracker.add_results(1)?;
            tracker.add_memory(locate_hit_memory(&hit)?)?;
            symbols.insert(hit.symbol_id);
            files.insert(hit.file_id);
            located.push(LocateHit {
                symbol: hit.symbol_id,
                file: hit.file_id,
                identifier: hit.identifier,
                qualified_name: hit.qualified_name,
                path: hit.path,
                kind: hit.kind,
                language: hit.language,
                tier: hit.tier,
                generated: hit.generated,
                relevance_score: hit.relevance_score,
                source: Some(source.clone()),
                trust: RepositoryDataTrust::UntrustedRepositoryData,
            });
        }

        let coverage = collect_coverage_partial(
            self.generation.document(),
            &symbols,
            &files,
            &mut tracker,
            &control,
            &mut limiting_resources,
        )?;
        let data = CodeLocateResult {
            generation: self.generation.metadata().generation(),
            hits: located,
            matched_candidates,
            coverage,
            truncated: !limiting_resources.is_empty(),
            limiting_resources,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic bounded `symbol.explain` plan.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid budgets or budgets too small for the
    /// mandatory entity and provenance records. Optional scans are capped and
    /// report explicit truncation at execution.
    pub fn plan_symbol_explain(
        &self,
        symbol: SymbolId,
        budget: QueryBudget,
    ) -> Result<SymbolExplainPlan, QueryError> {
        budget.validate()?;
        if budget.max_rows < 2 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Rows,
            });
        }
        if budget.max_results < 2 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        let estimate = PlanEstimate {
            rows: budget.max_rows,
            edges: budget.max_edges,
            results: budget.max_results,
            source_bytes: 0,
            // The normalized generation bounds every record, while the query
            // memory budget remains the conservative aggregate ceiling.
            memory_bytes: budget.max_memory_bytes,
            json_bytes: budget.max_json_bytes,
            estimated_tokens: budget.max_tokens,
            duration_micros: duration_micros(budget.max_duration),
        };
        ensure_estimate(estimate, budget)?;
        let explanation = PlanExplanation {
            generation: self.generation.metadata().generation(),
            kind: PlanKind::SymbolExplain,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::EntityLookup,
                QueryOperator::RelationScan,
                QueryOperator::OccurrenceScan,
                QueryOperator::ProvenanceLookup,
                QueryOperator::CoverageProjection,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(SymbolExplainPlan {
            symbol,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `symbol.explain` plan.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, a missing symbol or
    /// provenance record, generation drift, encoding, or resource exhaustion.
    pub fn execute_symbol_explain(
        &self,
        plan: &SymbolExplainPlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<SymbolExplainResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let document = self.generation.document();
        let entity = find_entity(document, plan.symbol).ok_or(QueryError::SymbolNotFound)?;
        let mut tracker = UsageTracker::new(plan.budget);
        tracker.add_rows(1)?;
        tracker.add_results(1)?;
        tracker.add_memory(serialized_size(
            entity,
            plan.budget.max_memory_bytes,
            &control,
        )?)?;

        let provenance = document
            .provenance
            .binary_search_by_key(&entity.provenance, |record| record.id)
            .ok()
            .and_then(|index| document.provenance.get(index))
            .ok_or(QueryError::ProvenanceMissing)?;
        tracker.add_rows(1)?;
        tracker.add_results(1)?;
        tracker.add_memory(serialized_size(
            provenance,
            tracker.remaining_memory(),
            &control,
        )?)?;

        let mut limiting_resources = Vec::new();
        let mut relations = Vec::new();
        for relation in &document.relations {
            control.check()?;
            if !tracker.can_add(QueryResource::Rows, 1) {
                record_limit(&mut limiting_resources, QueryResource::Rows)?;
                break;
            }
            if !tracker.can_add(QueryResource::Edges, 1) {
                record_limit(&mut limiting_resources, QueryResource::Edges)?;
                break;
            }
            tracker.add_rows(1)?;
            tracker.add_edges(1)?;
            if endpoint_matches(relation.subject, plan.symbol)
                || endpoint_matches(relation.object, plan.symbol)
            {
                if !tracker.can_add(QueryResource::Results, 1) {
                    record_limit(&mut limiting_resources, QueryResource::Results)?;
                    break;
                }
                let bytes = serialized_size(relation, u64::MAX, &control)?;
                if !tracker.can_add(QueryResource::MemoryBytes, bytes) {
                    record_limit(&mut limiting_resources, QueryResource::MemoryBytes)?;
                    break;
                }
                tracker.add_results(1)?;
                tracker.add_memory(bytes)?;
                try_push(&mut relations, relation.clone())?;
            }
        }

        let mut occurrences = Vec::new();
        if !limits_optional_results(&limiting_resources) {
            for occurrence in &document.occurrences {
                control.check()?;
                if !tracker.can_add(QueryResource::Rows, 1) {
                    record_limit(&mut limiting_resources, QueryResource::Rows)?;
                    break;
                }
                tracker.add_rows(1)?;
                if occurrence_matches(occurrence, plan.symbol) {
                    if !tracker.can_add(QueryResource::Results, 1) {
                        record_limit(&mut limiting_resources, QueryResource::Results)?;
                        break;
                    }
                    let bytes = serialized_size(occurrence, u64::MAX, &control)?;
                    if !tracker.can_add(QueryResource::MemoryBytes, bytes) {
                        record_limit(&mut limiting_resources, QueryResource::MemoryBytes)?;
                        break;
                    }
                    tracker.add_results(1)?;
                    tracker.add_memory(bytes)?;
                    try_push(&mut occurrences, occurrence.clone())?;
                }
            }
        }

        let symbols = BTreeSet::from([plan.symbol]);
        let files = entity
            .evidence
            .source
            .as_ref()
            .map(|source| BTreeSet::from([source.span().file()]))
            .unwrap_or_default();
        let coverage = if limits_optional_results(&limiting_resources) {
            Vec::new()
        } else {
            collect_coverage_partial(
                document,
                &symbols,
                &files,
                &mut tracker,
                &control,
                &mut limiting_resources,
            )?
        };
        let data = SymbolExplainResult {
            generation: self.generation.metadata().generation(),
            entity: entity.clone(),
            relations,
            occurrences,
            provenance: provenance.clone(),
            coverage,
            truncated: !limiting_resources.is_empty(),
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic bounded `symbol.relationships` plan.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, empty or oversized seed or
    /// relation-family sets, an out-of-range confidence threshold or result
    /// bound, arithmetic overflow, or a conservative estimate that cannot be
    /// admitted.
    pub fn plan_symbol_relationships(
        &self,
        seeds: BTreeSet<SymbolId>,
        families: Vec<RelationFamily>,
        direction: Option<RelationDirection>,
        min_confidence: u16,
        max_results: usize,
        budget: QueryBudget,
    ) -> Result<SymbolRelationshipsPlan, QueryError> {
        budget.validate()?;
        if seeds.is_empty() || seeds.len() > 64 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if families.is_empty() || families.len() > 16 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if min_confidence > 1_000 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_results == 0
            || max_results > 500
            || checked_usize_to_u64(max_results)? > budget.max_results
        {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        let estimate = PlanEstimate {
            rows: budget.max_rows,
            edges: budget.max_edges,
            results: budget.max_results,
            source_bytes: 0,
            // The normalized generation bounds every record, while the query
            // memory budget remains the conservative aggregate ceiling.
            memory_bytes: budget.max_memory_bytes,
            json_bytes: budget.max_json_bytes,
            estimated_tokens: budget.max_tokens,
            duration_micros: duration_micros(budget.max_duration),
        };
        ensure_estimate(estimate, budget)?;
        let explanation = PlanExplanation {
            generation: self.generation.metadata().generation(),
            kind: PlanKind::SymbolRelationships,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::RelationScan,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(SymbolRelationshipsPlan {
            seeds,
            families,
            direction,
            min_confidence,
            max_results,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `symbol.relationships` plan.
    ///
    /// The scan expands each requested relation family around every seed,
    /// keeping qualifying edges under the result bound and measuring rows,
    /// edges, results, and memory exactly like `symbol.explain`. Groups are
    /// keyed by seed, family, and effective direction so a `both` traversal
    /// reports each edge under the direction it actually matched.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, generation drift, encoding, or
    /// resource exhaustion.
    pub fn execute_symbol_relationships(
        &self,
        plan: &SymbolRelationshipsPlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<SymbolRelationshipsResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let document = self.generation.document();
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let max_results_bound = checked_usize_to_u64(plan.max_results)?;

        let mut groups: BTreeMap<(SymbolId, RelationFamily, RelationDirection), RelationshipGroup> =
            BTreeMap::new();
        let mut returned_edges: u64 = 0;
        let mut total_edges: u64 = 0;
        let mut truncated = false;

        'scan: for family in &plan.families {
            let predicates = family.predicates();
            if predicates.is_empty() {
                // The first-slice oracle has no data for this family; an honest
                // empty result is safer than fabricated edges.
                continue;
            }
            let effective = plan.direction.unwrap_or_else(|| family.natural_direction());
            for relation in &document.relations {
                control.check()?;
                if !tracker.can_add(QueryResource::Rows, 1) {
                    record_limit(&mut limiting_resources, QueryResource::Rows)?;
                    truncated = true;
                    break 'scan;
                }
                if !tracker.can_add(QueryResource::Edges, 1) {
                    record_limit(&mut limiting_resources, QueryResource::Edges)?;
                    truncated = true;
                    break 'scan;
                }
                tracker.add_rows(1)?;
                tracker.add_edges(1)?;
                if !predicates.contains(&relation.predicate) {
                    continue;
                }
                for (seed, direction, target) in
                    relation_candidates(document, relation, &plan.seeds, effective)
                {
                    let confidence = relation.confidence.get();
                    if confidence < plan.min_confidence {
                        continue;
                    }
                    let key = (seed, *family, direction);
                    total_edges = total_edges.saturating_add(1);
                    let group = groups.entry(key).or_insert_with(|| RelationshipGroup {
                        seed,
                        family: *family,
                        direction,
                        items: Vec::new(),
                        total_count: 0,
                    });
                    group.total_count = group.total_count.saturating_add(1);
                    if returned_edges >= max_results_bound {
                        record_limit(&mut limiting_resources, QueryResource::Results)?;
                        truncated = true;
                        break 'scan;
                    }
                    if !tracker.can_add(QueryResource::Results, 1) {
                        record_limit(&mut limiting_resources, QueryResource::Results)?;
                        truncated = true;
                        break 'scan;
                    }
                    let bytes = serialized_size(relation, u64::MAX, &control)?;
                    if !tracker.can_add(QueryResource::MemoryBytes, bytes) {
                        record_limit(&mut limiting_resources, QueryResource::MemoryBytes)?;
                        truncated = true;
                        break 'scan;
                    }
                    tracker.add_results(1)?;
                    tracker.add_memory(bytes)?;
                    group.items.push(RelationshipEdgeTarget {
                        symbol: target,
                        confidence,
                        source_refs: relation.evidence.source.iter().cloned().collect(),
                    });
                    returned_edges = returned_edges.saturating_add(1);
                }
            }
        }

        let mut groups: Vec<RelationshipGroup> = groups.into_values().collect();
        for group in &mut groups {
            group.items.sort_by(|left, right| {
                left.symbol
                    .cmp(&right.symbol)
                    .then_with(|| right.confidence.cmp(&left.confidence))
            });
        }
        let data = SymbolRelationshipsResult {
            generation: self.generation.metadata().generation(),
            groups,
            returned_edges: u32::try_from(returned_edges).unwrap_or(u32::MAX),
            total_edges: u32::try_from(total_edges).unwrap_or(u32::MAX),
            exact: !truncated,
            truncated,
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic bounded `flow.trace` plan.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, empty or oversized
    /// relation-family set, out-of-range confidence, depth, or path bounds,
    /// arithmetic overflow, or a conservative estimate that cannot be admitted.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded flow trace dimension"
    )]
    pub fn plan_flow_trace(
        &self,
        from: SymbolId,
        to: Option<SymbolId>,
        direction: Option<RelationDirection>,
        mut families: Vec<RelationFamily>,
        min_confidence: u16,
        max_depth: u8,
        max_paths: usize,
        budget: QueryBudget,
    ) -> Result<FlowTracePlan, QueryError> {
        budget.validate()?;
        if families.is_empty() || families.len() > 16 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if min_confidence > 1_000 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_depth == 0 || max_depth > 8 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_paths == 0
            || max_paths > 100
            || checked_usize_to_u64(max_paths)? > budget.max_results
        {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        families.sort();
        families.dedup();
        let estimate = PlanEstimate {
            rows: budget.max_rows,
            edges: budget.max_edges,
            results: budget.max_results,
            source_bytes: 0,
            // The normalized generation bounds every record, while the query
            // memory budget remains the conservative aggregate ceiling.
            memory_bytes: budget.max_memory_bytes,
            json_bytes: budget.max_json_bytes,
            estimated_tokens: budget.max_tokens,
            duration_micros: duration_micros(budget.max_duration),
        };
        ensure_estimate(estimate, budget)?;
        let explanation = PlanExplanation {
            generation: self.generation.metadata().generation(),
            kind: PlanKind::FlowTrace,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::RelationScan,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(FlowTracePlan {
            from,
            to,
            direction: direction.unwrap_or(RelationDirection::Outbound),
            families,
            min_confidence,
            max_depth,
            max_paths,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `flow.trace` plan.
    ///
    /// The scan builds a directed adjacency view over the requested relation
    /// projection, then enumerates bounded paths from the source node up to the
    /// configured depth and path cap, measuring rows, edges, results, and
    /// memory exactly like `symbol.relationships`. Without a target the trace
    /// reports bounded outward paths to every reached node; with a target it
    /// reports only paths that reach it.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, generation drift, encoding, or
    /// resource exhaustion.
    pub fn execute_flow_trace(
        &self,
        plan: &FlowTracePlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<FlowTraceResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let document = self.generation.document();
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();

        let (adjacency, scan_truncated) = build_flow_adjacency(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )?;
        let (paths, mut frontier) = trace_flow(
            &adjacency,
            plan.from,
            plan.to,
            plan.max_depth,
            plan.max_paths,
            &mut tracker,
            &mut limiting_resources,
            &control,
        )?;
        if scan_truncated {
            frontier.truncated = true;
        }

        let data = FlowTraceResult {
            generation: self.generation.metadata().generation(),
            paths,
            frontier,
            projection: FlowTraceProjection {
                families: plan.families.clone(),
                min_confidence: plan.min_confidence,
            },
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic bounded `architecture.cycles` plan.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, empty or oversized
    /// relation-family set, out-of-range confidence, component-size, or cycle
    /// bounds, arithmetic overflow, or a conservative estimate that cannot be
    /// admitted.
    pub fn plan_architecture_cycles(
        &self,
        mut families: Vec<RelationFamily>,
        min_confidence: u16,
        min_size: u8,
        max_cycles: usize,
        include_self_cycles: bool,
        budget: QueryBudget,
    ) -> Result<ArchitectureCyclesPlan, QueryError> {
        budget.validate()?;
        if families.is_empty() || families.len() > 8 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if min_confidence > 1_000 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if !(2..=64).contains(&min_size) {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_cycles == 0
            || max_cycles > 200
            || checked_usize_to_u64(max_cycles)? > budget.max_results
        {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        families.sort();
        families.dedup();
        let estimate = PlanEstimate {
            rows: budget.max_rows,
            edges: budget.max_edges,
            results: budget.max_results,
            source_bytes: 0,
            // The normalized generation bounds every record, while the query
            // memory budget remains the conservative aggregate ceiling.
            memory_bytes: budget.max_memory_bytes,
            json_bytes: budget.max_json_bytes,
            estimated_tokens: budget.max_tokens,
            duration_micros: duration_micros(budget.max_duration),
        };
        ensure_estimate(estimate, budget)?;
        let explanation = PlanExplanation {
            generation: self.generation.metadata().generation(),
            kind: PlanKind::ArchitectureCycles,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::RelationScan,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(ArchitectureCyclesPlan {
            families,
            min_confidence,
            min_size,
            max_cycles,
            include_self_cycles,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `architecture.cycles` plan.
    ///
    /// The scan builds a directed adjacency view over the requested relation
    /// projection, runs an iterative Tarjan strongly-connected-component pass
    /// to avoid recursion depth issues on large graphs, then extracts one
    /// bounded representative minimal cycle and one cheapest break candidate
    /// per reported component. Rows, edges, results, and memory are measured
    /// exactly like `flow.trace`.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, generation drift, encoding, or
    /// resource exhaustion.
    pub fn execute_architecture_cycles(
        &self,
        plan: &ArchitectureCyclesPlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ArchitectureCyclesResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let document = self.generation.document();
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();

        let adjacency = build_cycle_adjacency(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )?;
        let (components, cycles, break_candidates) = detect_cycles(
            &adjacency,
            plan,
            &mut tracker,
            &mut limiting_resources,
            &control,
        )?;

        let data = ArchitectureCyclesResult {
            generation: self.generation.metadata().generation(),
            components,
            cycles,
            break_candidates,
            projection: ArchitectureCyclesProjection {
                families: plan.families.clone(),
                min_confidence: plan.min_confidence,
            },
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic bounded `code.dead` plan.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, out-of-range confidence or
    /// candidate bounds, arithmetic overflow, or a conservative estimate that
    /// cannot be admitted.
    pub fn plan_code_dead(
        &self,
        entry_point_policy: CodeDeadEntryPointPolicy,
        include_exported: bool,
        include_tests: bool,
        min_confidence: u16,
        max_candidates: usize,
        budget: QueryBudget,
    ) -> Result<CodeDeadPlan, QueryError> {
        budget.validate()?;
        if min_confidence > 1_000 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_candidates == 0
            || max_candidates > 500
            || checked_usize_to_u64(max_candidates)? > budget.max_results
        {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        let estimate = PlanEstimate {
            rows: budget.max_rows,
            edges: budget.max_edges,
            results: budget.max_results,
            source_bytes: 0,
            // The normalized generation bounds every record, while the query
            // memory budget remains the conservative aggregate ceiling.
            memory_bytes: budget.max_memory_bytes,
            json_bytes: budget.max_json_bytes,
            estimated_tokens: budget.max_tokens,
            duration_micros: duration_micros(budget.max_duration),
        };
        ensure_estimate(estimate, budget)?;
        let explanation = PlanExplanation {
            generation: self.generation.metadata().generation(),
            kind: PlanKind::CodeDead,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::RelationScan,
                QueryOperator::EntityLookup,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(CodeDeadPlan {
            entry_point_policy,
            include_exported,
            include_tests,
            min_confidence,
            max_candidates,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `code.dead` plan.
    ///
    /// The scan builds a directed call/use adjacency view over the served
    /// reachability predicates, resolves an honest partial entry-point model
    /// from exported and test symbols, runs a forward reachability closure from
    /// the entry points, and classifies every unreached graph symbol. Rows,
    /// edges, results, and memory are measured exactly like
    /// `architecture.cycles`.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, generation drift, encoding, or
    /// resource exhaustion.
    pub fn execute_code_dead(
        &self,
        plan: &CodeDeadPlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<CodeDeadResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let document = self.generation.document();
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();

        let graph = build_dead_graph(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )?;
        let analysis = analyze_dead_code(
            document,
            &graph,
            plan,
            &mut tracker,
            &mut limiting_resources,
            &control,
        )?;

        let data = CodeDeadResult {
            generation: self.generation.metadata().generation(),
            candidates: analysis.candidates,
            entry_points: analysis.entry_points,
            blind_spots: analysis.blind_spots,
            suppression_rules: analysis.suppression_rules,
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic generation-bound `source.read` plan.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid budgets, foreign selectors, or a
    /// conservative source estimate that cannot be admitted.
    pub fn plan_source_read(
        &self,
        references: Vec<SourceRef>,
        options: SourceReadOptions,
        mut source_budget: SourceBudget,
        budget: QueryBudget,
    ) -> Result<SourceReadPlan, QueryError> {
        budget.validate()?;
        source_budget.max_duration = source_budget.max_duration.min(budget.max_duration);
        source_budget.validate()?;
        if references.is_empty() || references.len() > source_budget.max_selectors {
            return Err(QueryError::Source(SourceError::SelectorLimit));
        }
        if options.context_lines_before > source_budget.max_context_lines
            || options.context_lines_after > source_budget.max_context_lines
        {
            return Err(QueryError::Source(SourceError::ContextLimit));
        }
        for reference in &references {
            if reference.generation() != self.generation.metadata().generation()
                || reference.repository() != self.generation.metadata().repository()
            {
                return Err(QueryError::GenerationMismatch);
            }
        }
        let chunk_memory = checked_usize_to_u64(
            references
                .len()
                .checked_mul(mem::size_of::<SourceChunkResult>())
                .ok_or(QueryError::MemoryUnavailable)?,
        )?;
        let memory_bytes = checked_add(
            checked_usize_to_u64(source_budget.max_response_memory_bytes)?,
            chunk_memory,
            QueryResource::MemoryBytes,
            u64::MAX,
        )?;
        let estimate = PlanEstimate {
            rows: checked_usize_to_u64(references.len())?,
            edges: 0,
            results: checked_usize_to_u64(references.len())?,
            source_bytes: checked_usize_to_u64(source_budget.max_source_bytes)?,
            memory_bytes,
            json_bytes: budget.max_json_bytes,
            estimated_tokens: budget.max_tokens,
            duration_micros: duration_micros(budget.max_duration),
        };
        ensure_estimate(estimate, budget)?;
        let explanation = PlanExplanation {
            generation: self.generation.metadata().generation(),
            kind: PlanKind::SourceRead,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::SourceResolve,
                QueryOperator::VfsSnapshotRead,
                QueryOperator::ContentHashVerify,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(SourceReadPlan {
            references,
            options,
            source_budget,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `source.read` plan through the source service.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, source or generation drift,
    /// invalid UTF-8, encoding, or resource exhaustion.
    pub fn execute_source_read(
        &self,
        plan: &SourceReadPlan,
        source: &SourceService<'_>,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<SourceReadQueryResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let result = source.read(
            &plan.references,
            plan.options,
            plan.source_budget,
            cancellation,
        )?;
        control.check()?;
        if result.generation != self.generation.metadata().generation() {
            return Err(QueryError::GenerationMismatch);
        }
        let mut tracker = UsageTracker::new(plan.budget);
        tracker.add_rows(checked_usize_to_u64(plan.references.len())?)?;
        tracker.add_source_bytes(checked_usize_to_u64(result.total_source_bytes)?)?;
        tracker.add_memory(checked_usize_to_u64(result.total_response_memory_bytes)?)?;
        tracker.add_memory(checked_usize_to_u64(
            result
                .chunks
                .len()
                .checked_mul(mem::size_of::<SourceChunkResult>())
                .ok_or(QueryError::MemoryUnavailable)?,
        )?)?;
        let mut chunks = Vec::new();
        try_reserve(&mut chunks, result.chunks.len())?;
        for chunk in result.chunks {
            control.check()?;
            tracker.add_results(1)?;
            let text =
                String::from_utf8(chunk.bytes).map_err(|_| QueryError::InvalidSourceEncoding)?;
            chunks.push(SourceChunkResult {
                reference: chunk.reference,
                path: chunk.path,
                start_byte: chunk.start_byte,
                end_byte: chunk.end_byte,
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                text,
                content_hash: chunk.content_hash,
                language: chunk.language,
                generated: chunk.generated,
                trust: RepositoryDataTrust::UntrustedRepositoryData,
            });
        }
        let data = SourceReadQueryResult {
            generation: result.generation,
            chunks,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    fn require_generation(&self, generation: GenerationId) -> Result<(), QueryError> {
        if generation != self.generation.metadata().generation()
            || generation != self.search.generation()
        {
            Err(QueryError::GenerationMismatch)
        } else {
            Ok(())
        }
    }
}

impl<Search> std::fmt::Debug for QueryService<'_, Search>
where
    Search: LexicalSearch,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QueryService")
            .field("generation", &self.generation.metadata().generation())
            .finish_non_exhaustive()
    }
}

fn find_entity(
    document: &NormalizedIrDocument,
    symbol: SymbolId,
) -> Option<&rootlight_ir::EntityRecord> {
    document
        .entities
        .binary_search_by_key(&symbol, |entity| entity.id)
        .ok()
        .and_then(|index| document.entities.get(index))
}

fn find_file(document: &NormalizedIrDocument, file: FileId) -> Option<&rootlight_ir::FileRecord> {
    document
        .files
        .binary_search_by_key(&file, |record| record.id)
        .ok()
        .and_then(|index| document.files.get(index))
}

fn serialized_label(value: &impl Serialize) -> Result<String, QueryError> {
    let encoded = serde_json::to_string(value).map_err(|_| QueryError::ResultEncoding)?;
    encoded
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map(str::to_owned)
        .ok_or(QueryError::IndexDrift)
}

fn endpoint_matches(endpoint: RelationEndpoint, symbol: SymbolId) -> bool {
    endpoint == RelationEndpoint::Entity(symbol)
}

/// Expands one relation into its matching `(seed, direction, target)` candidate
/// edges for the requested seed set under an effective traversal direction.
///
/// Each endpoint contributes its effective entity: a direct entity endpoint
/// contributes itself, while an occurrence endpoint contributes its enclosing
/// entity. This lets a seed function match the call, reference, and type-use
/// occurrences the oracle records against it, and lets the opposite endpoint
/// report the related entity. Repository and file endpoints contribute nothing
/// because they are not relationship targets. A `both` traversal reports each
/// matched edge under the direction it actually satisfied, so a caller can
/// group inbound and outbound edges separately.
fn relation_candidates(
    document: &NormalizedIrDocument,
    relation: &rootlight_ir::RelationRecord,
    seeds: &BTreeSet<SymbolId>,
    effective: RelationDirection,
) -> Vec<(SymbolId, RelationDirection, SymbolId)> {
    let subject = endpoint_entity(document, relation.subject);
    let object = endpoint_entity(document, relation.object);
    let mut candidates = Vec::new();
    match effective {
        RelationDirection::Outbound => {
            if let (Some(seed), Some(target)) = (subject, object)
                && seeds.contains(&seed)
            {
                candidates.push((seed, RelationDirection::Outbound, target));
            }
        }
        RelationDirection::Inbound => {
            if let (Some(seed), Some(target)) = (object, subject)
                && seeds.contains(&seed)
            {
                candidates.push((seed, RelationDirection::Inbound, target));
            }
        }
        RelationDirection::Both => {
            if let (Some(seed), Some(target)) = (subject, object)
                && seeds.contains(&seed)
            {
                candidates.push((seed, RelationDirection::Outbound, target));
            }
            if let (Some(seed), Some(target)) = (object, subject)
                && seeds.contains(&seed)
            {
                candidates.push((seed, RelationDirection::Inbound, target));
            }
        }
    }
    candidates
}

/// Resolves one relation endpoint to its effective entity, when present.
fn endpoint_entity(
    document: &NormalizedIrDocument,
    endpoint: RelationEndpoint,
) -> Option<SymbolId> {
    match endpoint {
        RelationEndpoint::Entity(symbol) => Some(symbol),
        RelationEndpoint::Occurrence(occurrence) => occurrence_enclosing(document, occurrence),
        RelationEndpoint::Repository(_) | RelationEndpoint::File(_) => None,
    }
}

/// Returns the enclosing entity recorded for one occurrence, when present.
fn occurrence_enclosing(document: &NormalizedIrDocument, occurrence: FactId) -> Option<SymbolId> {
    document
        .occurrences
        .binary_search_by_key(&occurrence, |record| record.id)
        .ok()
        .and_then(|index| document.occurrences.get(index))
        .and_then(|record| record.enclosing)
}

/// One directed adjacency edge used by a `flow.trace` traversal.
#[derive(Debug, Clone)]
struct FlowAdjEdge {
    target: SymbolId,
    family: RelationFamily,
    confidence: u16,
    source_refs: Vec<SourceRef>,
}

/// Returns the first requested family admitting a predicate, in plan order.
///
/// The plan families are sorted and deduplicated, so the first match is
/// deterministic even when several requested families share a predicate (for
/// example `calls` and `called_by` both admit the `Calls` predicate).
fn predicate_family(
    families: &[RelationFamily],
    predicate: RelationPredicate,
) -> Option<RelationFamily> {
    families
        .iter()
        .copied()
        .find(|family| family.predicates().contains(&predicate))
}

/// Builds a directed adjacency view over the requested relation projection.
///
/// Each relation whose predicate is admitted by the projection and whose
/// confidence clears the threshold contributes entity-to-entity edges honoring
/// the traversal direction. Repository and file endpoints and occurrence-less
/// endpoints contribute nothing. The returned flag reports whether the relation
/// scan was cut short by a row or edge budget.
fn build_flow_adjacency(
    document: &NormalizedIrDocument,
    plan: &FlowTracePlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<(BTreeMap<SymbolId, Vec<FlowAdjEdge>>, bool), QueryError> {
    let allowed: BTreeSet<RelationPredicate> = plan
        .families
        .iter()
        .flat_map(|family| family.predicates().iter().copied())
        .collect();
    let mut adjacency: BTreeMap<SymbolId, Vec<FlowAdjEdge>> = BTreeMap::new();
    if allowed.is_empty() {
        return Ok((adjacency, false));
    }
    let mut scan_truncated = false;
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            scan_truncated = true;
            break;
        }
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            scan_truncated = true;
            break;
        }
        tracker.add_rows(1)?;
        tracker.add_edges(1)?;
        if !allowed.contains(&relation.predicate) {
            continue;
        }
        let confidence = relation.confidence.get();
        if confidence < plan.min_confidence {
            continue;
        }
        let Some(family) = predicate_family(&plan.families, relation.predicate) else {
            continue;
        };
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        let source_refs: Vec<SourceRef> = relation.evidence.source.iter().cloned().collect();
        match plan.direction {
            RelationDirection::Outbound => {
                adjacency.entry(subject).or_default().push(FlowAdjEdge {
                    target: object,
                    family,
                    confidence,
                    source_refs,
                })
            }
            RelationDirection::Inbound => adjacency.entry(object).or_default().push(FlowAdjEdge {
                target: subject,
                family,
                confidence,
                source_refs,
            }),
            RelationDirection::Both => {
                adjacency.entry(subject).or_default().push(FlowAdjEdge {
                    target: object,
                    family,
                    confidence,
                    source_refs: source_refs.clone(),
                });
                adjacency.entry(object).or_default().push(FlowAdjEdge {
                    target: subject,
                    family,
                    confidence,
                    source_refs,
                });
            }
        }
    }
    for edges in adjacency.values_mut() {
        edges.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| left.family.as_str().cmp(right.family.as_str()))
                .then_with(|| right.confidence.cmp(&left.confidence))
        });
    }
    Ok((adjacency, scan_truncated))
}

/// Mutable state threaded through the bounded `flow.trace` depth-first walk.
struct FlowWalkState<'tracker, 'limits> {
    tracker: &'tracker mut UsageTracker,
    limiting_resources: &'limits mut Vec<QueryResource>,
    paths: Vec<FlowTracePath>,
    reached: BTreeSet<SymbolId>,
    examined_edges: u64,
    truncated: bool,
    depth_cut: bool,
}

/// Enumerates bounded paths from `from` over the adjacency view.
///
/// Without a target, every prefix path from the source to a reached node is
/// reported; with a target, only paths that reach it are reported. Branches
/// stop at the depth bound, the path cap, a budget limit, or a cycle (the
/// cycle-closing path is still reported with `cyclic` set).
#[expect(
    clippy::too_many_arguments,
    reason = "the trace entry point carries its bounded budget and control state"
)]
fn trace_flow(
    adjacency: &BTreeMap<SymbolId, Vec<FlowAdjEdge>>,
    from: SymbolId,
    to: Option<SymbolId>,
    max_depth: u8,
    max_paths: usize,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<(Vec<FlowTracePath>, FlowTraceFrontier), QueryError> {
    let mut state = FlowWalkState {
        tracker,
        limiting_resources,
        paths: Vec::new(),
        reached: BTreeSet::new(),
        examined_edges: 0,
        truncated: false,
        depth_cut: false,
    };
    let mut path_nodes = vec![from];
    let mut path_edges = Vec::new();
    walk_flow(
        adjacency,
        to,
        max_depth,
        max_paths,
        from,
        &mut path_nodes,
        &mut path_edges,
        false,
        &mut state,
        control,
    )?;

    state.paths.sort_by(|left, right| {
        left.nodes.cmp(&right.nodes).then_with(|| {
            let left_key: Vec<(&str, u16)> = left
                .edges
                .iter()
                .map(|edge| (edge.family.as_str(), edge.confidence))
                .collect();
            let right_key: Vec<(&str, u16)> = right
                .edges
                .iter()
                .map(|edge| (edge.family.as_str(), edge.confidence))
                .collect();
            left_key.cmp(&right_key)
        })
    });

    let mut unresolved_boundaries: usize = 0;
    for node in &state.reached {
        if let Some(edges) = adjacency.get(node)
            && edges
                .iter()
                .any(|edge| !state.reached.contains(&edge.target))
        {
            unresolved_boundaries = unresolved_boundaries.saturating_add(1);
        }
    }

    let frontier = FlowTraceFrontier {
        reached_nodes: u32::try_from(state.reached.len()).unwrap_or(u32::MAX),
        examined_edges: u32::try_from(state.examined_edges).unwrap_or(u32::MAX),
        truncated: state.truncated || state.depth_cut,
        unresolved_boundaries: u32::try_from(unresolved_boundaries).unwrap_or(u32::MAX),
    };
    Ok((state.paths, frontier))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the recursive walk carries its bounded path and budget state"
)]
fn walk_flow(
    adjacency: &BTreeMap<SymbolId, Vec<FlowAdjEdge>>,
    to: Option<SymbolId>,
    max_depth: u8,
    max_paths: usize,
    node: SymbolId,
    path_nodes: &mut Vec<SymbolId>,
    path_edges: &mut Vec<FlowTraceEdge>,
    cyclic: bool,
    state: &mut FlowWalkState<'_, '_>,
    control: &QueryControl<'_>,
) -> Result<(), QueryError> {
    state.reached.insert(node);
    control.check()?;

    let at_target = to.is_some_and(|target| target == node);
    if path_nodes.len() >= 2 && (at_target || to.is_none()) {
        emit_flow_path(state, path_nodes, path_edges, cyclic, control)?;
    }

    if cyclic || at_target {
        return Ok(());
    }
    if path_edges.len() >= usize::from(max_depth) {
        if adjacency.get(&node).is_some_and(|edges| !edges.is_empty()) {
            state.depth_cut = true;
        }
        return Ok(());
    }

    let Some(neighbors) = adjacency.get(&node) else {
        return Ok(());
    };
    for edge in neighbors {
        if state.paths.len() >= max_paths {
            state.truncated = true;
            return Ok(());
        }
        if !state.tracker.can_add(QueryResource::Edges, 1) {
            record_limit(state.limiting_resources, QueryResource::Edges)?;
            state.truncated = true;
            return Ok(());
        }
        state.tracker.add_edges(1)?;
        state.examined_edges = state.examined_edges.saturating_add(1);

        let next_cyclic = path_nodes.contains(&edge.target);
        path_nodes.push(edge.target);
        path_edges.push(FlowTraceEdge {
            family: edge.family,
            confidence: edge.confidence,
            source_refs: edge.source_refs.clone(),
        });
        walk_flow(
            adjacency,
            to,
            max_depth,
            max_paths,
            edge.target,
            path_nodes,
            path_edges,
            next_cyclic,
            state,
            control,
        )?;
        path_nodes.pop();
        path_edges.pop();
    }
    Ok(())
}

/// Records one emitted path under the result and memory budgets.
fn emit_flow_path(
    state: &mut FlowWalkState<'_, '_>,
    path_nodes: &[SymbolId],
    path_edges: &[FlowTraceEdge],
    cyclic: bool,
    control: &QueryControl<'_>,
) -> Result<(), QueryError> {
    if !state.tracker.can_add(QueryResource::Results, 1) {
        record_limit(state.limiting_resources, QueryResource::Results)?;
        state.truncated = true;
        return Ok(());
    }
    let path = FlowTracePath {
        confidence: path_edges
            .iter()
            .map(|edge| edge.confidence)
            .min()
            .unwrap_or_default(),
        nodes: path_nodes.to_vec(),
        edges: path_edges.to_vec(),
        cyclic,
    };
    let bytes = serialized_size(&path, u64::MAX, control)?;
    if !state.tracker.can_add(QueryResource::MemoryBytes, bytes) {
        record_limit(state.limiting_resources, QueryResource::MemoryBytes)?;
        state.truncated = true;
        return Ok(());
    }
    state.tracker.add_results(1)?;
    state.tracker.add_memory(bytes)?;
    state.paths.push(path);
    Ok(())
}

/// One directed adjacency edge used by an `architecture.cycles` detection.
#[derive(Debug, Clone)]
struct CycleAdjEdge {
    target: SymbolId,
    family: RelationFamily,
    confidence: u16,
    source_refs: Vec<SourceRef>,
}

/// Builds a directed outbound adjacency view over the requested projection.
///
/// Each served relation contributes a subject-to-object entity edge, including
/// self-edges, so cycle detection sees the raw directed dependency graph.
/// Repository and file endpoints and occurrence-less endpoints contribute
/// nothing. The scan is bounded by the same row and edge budgets as
/// `flow.trace`.
fn build_cycle_adjacency(
    document: &NormalizedIrDocument,
    plan: &ArchitectureCyclesPlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<BTreeMap<SymbolId, Vec<CycleAdjEdge>>, QueryError> {
    let allowed: BTreeSet<RelationPredicate> = plan
        .families
        .iter()
        .flat_map(|family| family.predicates().iter().copied())
        .collect();
    let mut adjacency: BTreeMap<SymbolId, Vec<CycleAdjEdge>> = BTreeMap::new();
    if allowed.is_empty() {
        return Ok(adjacency);
    }
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            break;
        }
        tracker.add_rows(1)?;
        tracker.add_edges(1)?;
        if !allowed.contains(&relation.predicate) {
            continue;
        }
        let confidence = relation.confidence.get();
        if confidence < plan.min_confidence {
            continue;
        }
        let Some(family) = predicate_family(&plan.families, relation.predicate) else {
            continue;
        };
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        let source_refs: Vec<SourceRef> = relation.evidence.source.iter().cloned().collect();
        adjacency.entry(subject).or_default().push(CycleAdjEdge {
            target: object,
            family,
            confidence,
            source_refs,
        });
    }
    for edges in adjacency.values_mut() {
        edges.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| left.family.as_str().cmp(right.family.as_str()))
                .then_with(|| right.confidence.cmp(&left.confidence))
        });
    }
    Ok(adjacency)
}

/// Detects strongly connected components, representative cycles, and break
/// candidates over the served adjacency view.
///
/// Components are reported when their size clears `min_size` (always at least
/// two), plus size-one self-cycles when explicitly requested. One bounded
/// representative minimal cycle and one cheapest break candidate are extracted
/// per reported component, all under the result and memory budgets.
type CycleDetection = (Vec<CycleComponent>, Vec<CyclePath>, Vec<CycleBreak>);

fn detect_cycles(
    adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
    plan: &ArchitectureCyclesPlan,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<CycleDetection, QueryError> {
    let mut nodes: BTreeSet<SymbolId> = BTreeSet::new();
    for (source, edges) in adjacency {
        nodes.insert(*source);
        for edge in edges {
            nodes.insert(edge.target);
        }
    }
    let raw_components = strongly_connected_components(adjacency, &nodes);

    let mut selected: Vec<Vec<SymbolId>> = Vec::new();
    for mut component in raw_components {
        component.sort();
        let size = component.len();
        let self_cycle = plan.include_self_cycles
            && size == 1
            && component
                .first()
                .is_some_and(|node| best_edge(adjacency, *node, *node).is_some());
        if (size >= 2 && size >= usize::from(plan.min_size)) || self_cycle {
            selected.push(component);
        }
    }
    selected.sort_by(|left, right| {
        right
            .len()
            .cmp(&left.len())
            .then_with(|| left[0].cmp(&right[0]))
    });
    if selected.len() > plan.max_cycles {
        selected.truncate(plan.max_cycles);
        record_limit(limiting_resources, QueryResource::Results)?;
    }

    let mut components: Vec<CycleComponent> = Vec::new();
    let mut cycles: Vec<CyclePath> = Vec::new();
    let mut break_candidates: Vec<CycleBreak> = Vec::new();

    for component in &selected {
        control.check()?;
        let member_set: BTreeSet<SymbolId> = component.iter().copied().collect();
        let mut internal_edges = 0_u32;
        for member in &member_set {
            if let Some(edges) = adjacency.get(member) {
                for edge in edges {
                    if member_set.contains(&edge.target) {
                        internal_edges = internal_edges.saturating_add(1);
                    }
                }
            }
        }
        let component_record = CycleComponent {
            size: u32::try_from(component.len()).unwrap_or(u32::MAX),
            members: component.clone(),
            internal_edges,
        };
        emit_cycle_value(
            &mut components,
            component_record,
            tracker,
            limiting_resources,
            control,
        )?;

        let cycle_nodes = if component.len() == 1 {
            let node = component[0];
            vec![node, node]
        } else {
            match representative_cycle(adjacency, &member_set, component[0]) {
                Some(path) => path,
                None => continue,
            }
        };
        let (confidence, edge_evidence) = cycle_details(adjacency, &cycle_nodes);
        let cycle_record = CyclePath {
            nodes: cycle_nodes.clone(),
            confidence,
            edge_evidence,
        };
        emit_cycle_value(
            &mut cycles,
            cycle_record,
            tracker,
            limiting_resources,
            control,
        )?;

        if let Some(break_record) = break_candidate(adjacency, &cycle_nodes) {
            emit_cycle_value(
                &mut break_candidates,
                break_record,
                tracker,
                limiting_resources,
                control,
            )?;
        }
    }

    cycles.sort_by(|left, right| left.nodes.cmp(&right.nodes));
    break_candidates.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then_with(|| left.to.cmp(&right.to))
            .then_with(|| left.family.as_str().cmp(right.family.as_str()))
    });

    Ok((components, cycles, break_candidates))
}

/// Records one emitted cycle artifact under the result and memory budgets.
fn emit_cycle_value<T>(
    values: &mut Vec<T>,
    value: T,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<(), QueryError>
where
    T: Serialize,
{
    if !tracker.can_add(QueryResource::Results, 1) {
        record_limit(limiting_resources, QueryResource::Results)?;
        return Ok(());
    }
    let bytes = serialized_size(&value, u64::MAX, control)?;
    if !tracker.can_add(QueryResource::MemoryBytes, bytes) {
        record_limit(limiting_resources, QueryResource::MemoryBytes)?;
        return Ok(());
    }
    tracker.add_results(1)?;
    tracker.add_memory(bytes)?;
    try_push(values, value)?;
    Ok(())
}

/// Runs an iterative Tarjan strongly-connected-component pass.
///
/// The explicit call stack avoids recursion depth issues on large dependency
/// graphs. Nodes are visited in deterministic sorted order and each component
/// is returned with its members in stack-pop order (callers sort them).
fn strongly_connected_components(
    adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
    nodes: &BTreeSet<SymbolId>,
) -> Vec<Vec<SymbolId>> {
    let mut index = 0_u32;
    let mut indices: BTreeMap<SymbolId, u32> = BTreeMap::new();
    let mut lowlinks: BTreeMap<SymbolId, u32> = BTreeMap::new();
    let mut stack: Vec<SymbolId> = Vec::new();
    let mut on_stack: BTreeSet<SymbolId> = BTreeSet::new();
    let mut components: Vec<Vec<SymbolId>> = Vec::new();

    for start in nodes {
        if indices.contains_key(start) {
            continue;
        }
        indices.insert(*start, index);
        lowlinks.insert(*start, index);
        index = index.saturating_add(1);
        stack.push(*start);
        on_stack.insert(*start);
        let mut call_stack: Vec<(SymbolId, usize)> = vec![(*start, 0)];
        while let Some(&(node, neighbor_index)) = call_stack.last() {
            let neighbor_count = adjacency.get(&node).map_or(0, Vec::len);
            if neighbor_index < neighbor_count {
                let target = adjacency[&node][neighbor_index].target;
                call_stack.last_mut().expect("the active frame exists").1 += 1;
                match indices.entry(target) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(index);
                        lowlinks.insert(target, index);
                        index = index.saturating_add(1);
                        stack.push(target);
                        on_stack.insert(target);
                        call_stack.push((target, 0));
                    }
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        if on_stack.contains(&target) {
                            let target_index = *entry.get();
                            let lowlink =
                                lowlinks.get_mut(&node).expect("visited node has a lowlink");
                            if target_index < *lowlink {
                                *lowlink = target_index;
                            }
                        }
                    }
                }
            } else {
                call_stack.pop();
                let node_lowlink = lowlinks[&node];
                let node_index = indices[&node];
                if node_lowlink == node_index {
                    let mut component = Vec::new();
                    loop {
                        let member = stack.pop().expect("the stack holds the component root");
                        on_stack.remove(&member);
                        component.push(member);
                        if member == node {
                            break;
                        }
                    }
                    components.push(component);
                }
                if let Some(&(parent, _)) = call_stack.last() {
                    let parent_lowlink = lowlinks
                        .get_mut(&parent)
                        .expect("visited parent has a lowlink");
                    if node_lowlink < *parent_lowlink {
                        *parent_lowlink = node_lowlink;
                    }
                }
            }
        }
    }
    components
}

/// Finds one bounded simple cycle through `start` inside a component.
///
/// A breadth-first search within the component looks for the shortest path
/// back to `start`, skipping self-edges so a multi-node component yields a
/// cycle through at least two distinct nodes. Neighbor order is deterministic
/// because the adjacency edges are pre-sorted.
fn representative_cycle(
    adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
    member_set: &BTreeSet<SymbolId>,
    start: SymbolId,
) -> Option<Vec<SymbolId>> {
    let mut parent: BTreeMap<SymbolId, SymbolId> = BTreeMap::new();
    let mut visited: BTreeSet<SymbolId> = BTreeSet::from([start]);
    let mut queue: VecDeque<SymbolId> = VecDeque::from([start]);
    while let Some(node) = queue.pop_front() {
        let neighbors = adjacency.get(&node).map(Vec::as_slice).unwrap_or(&[]);
        for edge in neighbors {
            let target = edge.target;
            if target == node || !member_set.contains(&target) {
                continue;
            }
            if target == start {
                let mut chain = vec![node];
                let mut cursor = node;
                while cursor != start {
                    cursor = *parent.get(&cursor)?;
                    chain.push(cursor);
                }
                chain.reverse();
                chain.push(start);
                return Some(chain);
            }
            if visited.insert(target) {
                parent.insert(target, node);
                queue.push_back(target);
            }
        }
    }
    None
}

/// Returns the strongest edge from one node to another, deterministically.
fn best_edge(
    adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
    from: SymbolId,
    to: SymbolId,
) -> Option<&CycleAdjEdge> {
    adjacency
        .get(&from)?
        .iter()
        .filter(|edge| edge.target == to)
        .max_by(|left, right| {
            left.confidence
                .cmp(&right.confidence)
                .then_with(|| left.family.as_str().cmp(right.family.as_str()))
        })
}

/// Computes the weakest-edge confidence and bounded evidence for a cycle.
fn cycle_details(
    adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
    nodes: &[SymbolId],
) -> (u16, Vec<SourceRef>) {
    const MAX_CYCLE_EVIDENCE: usize = 64;
    let mut confidence = u16::MAX;
    let mut evidence: Vec<SourceRef> = Vec::new();
    for pair in nodes.windows(2) {
        if let Some(edge) = best_edge(adjacency, pair[0], pair[1]) {
            confidence = confidence.min(edge.confidence);
            for source in &edge.source_refs {
                if evidence.len() < MAX_CYCLE_EVIDENCE {
                    evidence.push(source.clone());
                }
            }
        }
    }
    if confidence == u16::MAX {
        confidence = 0;
    }
    (confidence, evidence)
}

/// Selects the cheapest single edge whose removal breaks the cycle.
///
/// Lower confidence means a weaker, cheaper-to-break dependency, so the
/// lowest-confidence cycle edge is proposed and its confidence becomes the
/// break cost.
fn break_candidate(
    adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
    nodes: &[SymbolId],
) -> Option<CycleBreak> {
    const MAX_BREAK_REFS: usize = 8;
    let mut chosen: Option<(SymbolId, SymbolId, &CycleAdjEdge)> = None;
    for pair in nodes.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        if let Some(edge) = best_edge(adjacency, from, to) {
            let better = chosen.is_none_or(|(_, _, current)| edge.confidence < current.confidence);
            if better {
                chosen = Some((from, to, edge));
            }
        }
    }
    chosen.map(|(from, to, edge)| CycleBreak {
        from,
        to,
        family: edge.family,
        break_cost: edge.confidence,
        source_refs: edge
            .source_refs
            .iter()
            .take(MAX_BREAK_REFS)
            .cloned()
            .collect(),
    })
}

/// Relation families whose served predicates back the dead-code call/use graph.
///
/// The first-slice oracle records direct calls as `DispatchCandidate`
/// occurrences rather than entity-to-entity relations, so the reachability scan
/// also admits the `DispatchCandidate` predicate explicitly below. On a purely
/// lexical fixture no served predicate yields an entity-to-entity edge, so the
/// graph stays empty and `code.dead` honestly reports no proven candidates.
const CODE_DEAD_FAMILIES: &[RelationFamily] = &[
    RelationFamily::Calls,
    RelationFamily::References,
    RelationFamily::Imports,
];

/// One directed adjacency edge used by a `code.dead` reachability scan.
#[derive(Debug, Clone)]
struct DeadAdjEdge {
    target: SymbolId,
    confidence: u16,
}

/// Directed call/use graph plus per-symbol incoming statistics.
#[derive(Debug, Default)]
struct DeadGraph {
    /// Outbound adjacency from a subject symbol to its served targets.
    adjacency: BTreeMap<SymbolId, Vec<DeadAdjEdge>>,
    /// Every symbol appearing as a served relation endpoint.
    nodes: BTreeSet<SymbolId>,
    /// Incoming served-edge count per symbol.
    incoming_count: BTreeMap<SymbolId, u32>,
    /// Strongest incoming served-edge confidence per symbol.
    incoming_max_confidence: BTreeMap<SymbolId, u16>,
    /// Whether the relation scan was cut short by a row or edge budget.
    truncated: bool,
}

/// Honest result of the bounded dead-code reachability analysis.
struct DeadAnalysis {
    candidates: Vec<DeadCodeCandidate>,
    entry_points: CodeDeadEntryPointSummary,
    blind_spots: Vec<CodeDeadBlindSpot>,
    suppression_rules: Vec<CodeDeadSuppressionRule>,
}

/// Builds a directed call/use graph over the served reachability predicates.
///
/// Each served relation whose predicate is admitted and whose confidence clears
/// the threshold contributes a subject-to-object entity edge. Repository and
/// file endpoints and occurrence-less endpoints contribute nothing. The scan is
/// bounded by the same row and edge budgets as `architecture.cycles`.
fn build_dead_graph(
    document: &NormalizedIrDocument,
    plan: &CodeDeadPlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<DeadGraph, QueryError> {
    let mut allowed: BTreeSet<RelationPredicate> = CODE_DEAD_FAMILIES
        .iter()
        .flat_map(|family| family.predicates().iter().copied())
        .collect();
    // The first-slice oracle records direct calls as dispatch candidates; admit
    // them explicitly so a served call graph can form when the oracle provides
    // entity-to-entity dispatch relations.
    allowed.insert(RelationPredicate::DispatchCandidate);

    let mut graph = DeadGraph::default();
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            graph.truncated = true;
            break;
        }
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            graph.truncated = true;
            break;
        }
        tracker.add_rows(1)?;
        tracker.add_edges(1)?;
        if !allowed.contains(&relation.predicate) {
            continue;
        }
        let confidence = relation.confidence.get();
        if confidence < plan.min_confidence {
            continue;
        }
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        graph.nodes.insert(subject);
        graph.nodes.insert(object);
        graph
            .adjacency
            .entry(subject)
            .or_default()
            .push(DeadAdjEdge {
                target: object,
                confidence,
            });
        let count = graph.incoming_count.entry(object).or_insert(0);
        *count = count.saturating_add(1);
        let max_confidence = graph.incoming_max_confidence.entry(object).or_insert(0);
        if confidence > *max_confidence {
            *max_confidence = confidence;
        }
    }
    for edges in graph.adjacency.values_mut() {
        edges.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| right.confidence.cmp(&left.confidence))
        });
    }
    Ok(graph)
}

/// Resolves the entry-point model and classifies every unreached graph symbol.
///
/// Exported and test symbols are resolved from normalized entities and served
/// `Exports` relations under the row budget. By default those symbols are
/// protected as reachability roots; the include flags lift that protection so
/// the symbols can themselves be reported. The forward closure from the roots
/// marks every reachable symbol, and each remaining graph symbol is classified
/// by its incoming-edge evidence.
fn analyze_dead_code(
    document: &NormalizedIrDocument,
    graph: &DeadGraph,
    plan: &CodeDeadPlan,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<DeadAnalysis, QueryError> {
    let mut exported: BTreeSet<SymbolId> = BTreeSet::new();
    let mut tests: BTreeSet<SymbolId> = BTreeSet::new();
    for entity in &document.entities {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if entity_is_exported(entity) {
            exported.insert(entity.id);
        }
        if entity_is_test(entity) {
            tests.insert(entity.id);
        }
    }
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if relation.predicate != RelationPredicate::Exports {
            continue;
        }
        if let Some(symbol) = endpoint_entity(document, relation.object) {
            exported.insert(symbol);
        }
    }

    let mut entry_points: BTreeSet<SymbolId> = BTreeSet::new();
    let mut exported_suppressed = 0_u32;
    let mut test_suppressed = 0_u32;
    if !plan.include_exported {
        for symbol in &exported {
            if entry_points.insert(*symbol) {
                exported_suppressed = exported_suppressed.saturating_add(1);
            }
        }
    }
    if !plan.include_tests {
        for symbol in &tests {
            if entry_points.insert(*symbol) {
                test_suppressed = test_suppressed.saturating_add(1);
            }
        }
    }

    let candidates = detect_dead_candidates(
        document,
        graph,
        &entry_points,
        &exported,
        &tests,
        plan.max_candidates,
        tracker,
        limiting_resources,
        control,
    )?;

    let entry_point_count = u32::try_from(entry_points.len()).unwrap_or(u32::MAX);
    let analysis = DeadAnalysis {
        candidates,
        entry_points: CodeDeadEntryPointSummary {
            policy: plan.entry_point_policy,
            entry_point_count,
            // The first-slice entry-point model is always partial: dynamic
            // dispatch, reflection, and unindexed entry points are not provably
            // resolved.
            complete: false,
        },
        blind_spots: dead_blind_spots(plan, document, graph.truncated),
        suppression_rules: dead_suppression_rules(
            exported_suppressed,
            test_suppressed,
            entry_point_count,
        ),
    };
    Ok(analysis)
}

/// Runs a forward breadth-first reachability closure from the entry points.
fn reachability_closure(
    graph: &DeadGraph,
    entry_points: &BTreeSet<SymbolId>,
    control: &QueryControl<'_>,
) -> Result<BTreeSet<SymbolId>, QueryError> {
    let mut reached: BTreeSet<SymbolId> = BTreeSet::new();
    let mut queue: VecDeque<SymbolId> = VecDeque::new();
    for symbol in entry_points {
        if reached.insert(*symbol) {
            queue.push_back(*symbol);
        }
    }
    while let Some(node) = queue.pop_front() {
        control.check()?;
        let Some(edges) = graph.adjacency.get(&node) else {
            continue;
        };
        for edge in edges {
            if reached.insert(edge.target) {
                queue.push_back(edge.target);
            }
        }
    }
    Ok(reached)
}

/// Classifies every graph symbol unreached from the entry points.
///
/// The forward closure marks every symbol reachable from the protected roots;
/// each remaining graph symbol becomes a candidate ordered by stable identity
/// and capped at `max_candidates`. A symbol with no incoming served edges is
/// proven dead; an unreached symbol with incoming edges is probable or
/// suspected dead based on its strongest incoming confidence.
#[expect(
    clippy::too_many_arguments,
    reason = "the detection entry point carries its bounded budget and control state"
)]
fn detect_dead_candidates(
    document: &NormalizedIrDocument,
    graph: &DeadGraph,
    entry_points: &BTreeSet<SymbolId>,
    exported: &BTreeSet<SymbolId>,
    tests: &BTreeSet<SymbolId>,
    max_candidates: usize,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<Vec<DeadCodeCandidate>, QueryError> {
    let reached = reachability_closure(graph, entry_points, control)?;
    let mut candidate_symbols: Vec<SymbolId> = graph
        .nodes
        .iter()
        .copied()
        .filter(|symbol| !reached.contains(symbol) && !entry_points.contains(symbol))
        .collect();
    candidate_symbols.sort();

    let mut candidates: Vec<DeadCodeCandidate> = Vec::new();
    for symbol in candidate_symbols {
        control.check()?;
        if candidates.len() >= max_candidates {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        let incoming = graph.incoming_count.get(&symbol).copied().unwrap_or(0);
        let max_confidence = graph
            .incoming_max_confidence
            .get(&symbol)
            .copied()
            .unwrap_or(0);
        let classification = if incoming == 0 {
            DeadCodeClassification::ProvenDead
        } else if max_confidence >= 500 {
            DeadCodeClassification::ProbableDead
        } else {
            DeadCodeClassification::SuspectedDead
        };
        let confidence = match classification {
            DeadCodeClassification::ProvenDead => 1_000,
            DeadCodeClassification::ProbableDead => 700,
            DeadCodeClassification::SuspectedDead => 400,
        };
        let mut why = Vec::new();
        if incoming == 0 {
            why.push("no_incoming_references".to_owned());
        }
        why.push("unreachable_from_entry_points".to_owned());
        let candidate = DeadCodeCandidate {
            symbol_id: symbol,
            classification,
            confidence,
            why,
            suppressions_checked: suppressions_checked_for(symbol, exported, tests),
            source_refs: entity_source_refs(document, symbol),
        };
        emit_dead_candidate(
            &mut candidates,
            candidate,
            tracker,
            limiting_resources,
            control,
        )?;
    }
    Ok(candidates)
}

/// Returns whether one normalized entity belongs to the exported surface.
fn entity_is_exported(entity: &rootlight_ir::EntityRecord) -> bool {
    matches!(entity.kind, EntityKind::Export)
        || matches!(entity.visibility, EntityVisibility::Public)
        || entity.flags.contains(&EntityFlag::Exported)
}

/// Returns whether one normalized entity is test-only or test-related.
fn entity_is_test(entity: &rootlight_ir::EntityRecord) -> bool {
    matches!(entity.kind, EntityKind::Test) || entity.flags.contains(&EntityFlag::Test)
}

/// Returns bounded direct source evidence for one entity definition.
fn entity_source_refs(document: &NormalizedIrDocument, symbol: SymbolId) -> Vec<SourceRef> {
    const MAX_DEAD_SOURCE_REFS: usize = 8;
    find_entity(document, symbol)
        .and_then(|entity| entity.evidence.source.clone())
        .into_iter()
        .take(MAX_DEAD_SOURCE_REFS)
        .collect()
}

/// Returns the deterministic suppression rules checked for one candidate.
fn suppressions_checked_for(
    symbol: SymbolId,
    exported: &BTreeSet<SymbolId>,
    tests: &BTreeSet<SymbolId>,
) -> Vec<String> {
    let mut checked = Vec::new();
    checked.push("entry_point".to_owned());
    if exported.contains(&symbol) {
        checked.push("exported".to_owned());
    }
    if tests.contains(&symbol) {
        checked.push("test".to_owned());
    }
    checked
}

/// Builds the deterministic source-free blind-spot caveats for the analysis.
fn dead_blind_spots(
    plan: &CodeDeadPlan,
    document: &NormalizedIrDocument,
    scan_truncated: bool,
) -> Vec<CodeDeadBlindSpot> {
    let mut blind_spots = Vec::new();
    // Dynamic dispatch and reflection can reach symbols the static call graph
    // does not record, so an unreachable symbol may still be live at runtime.
    blind_spots.push(CodeDeadBlindSpot {
        category: "dynamic_dispatch".to_owned(),
        affected_count: 0,
    });
    let incomplete_coverage = u32::try_from(
        document
            .entities
            .iter()
            .filter(|entity| matches!(entity.tier, AnalysisTier::TierD))
            .count(),
    )
    .unwrap_or(u32::MAX);
    blind_spots.push(CodeDeadBlindSpot {
        category: "incomplete_language_coverage".to_owned(),
        affected_count: incomplete_coverage,
    });
    blind_spots.push(CodeDeadBlindSpot {
        category: "partial_entry_point_model".to_owned(),
        affected_count: 0,
    });
    if matches!(
        plan.entry_point_policy,
        CodeDeadEntryPointPolicy::Application
    ) {
        blind_spots.push(CodeDeadBlindSpot {
            category: "application_entry_points".to_owned(),
            affected_count: 0,
        });
    }
    if scan_truncated {
        blind_spots.push(CodeDeadBlindSpot {
            category: "budget_truncated_scan".to_owned(),
            affected_count: 0,
        });
    }
    blind_spots
}

/// Builds the deterministic applied suppression-rule summary.
fn dead_suppression_rules(
    exported_suppressed: u32,
    test_suppressed: u32,
    entry_point_count: u32,
) -> Vec<CodeDeadSuppressionRule> {
    vec![
        CodeDeadSuppressionRule {
            rule: "entry_point".to_owned(),
            suppressed_count: entry_point_count,
        },
        CodeDeadSuppressionRule {
            rule: "exported".to_owned(),
            suppressed_count: exported_suppressed,
        },
        CodeDeadSuppressionRule {
            rule: "test".to_owned(),
            suppressed_count: test_suppressed,
        },
    ]
}

/// Records one emitted dead-code candidate under the result and memory budgets.
fn emit_dead_candidate(
    candidates: &mut Vec<DeadCodeCandidate>,
    candidate: DeadCodeCandidate,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<(), QueryError> {
    if !tracker.can_add(QueryResource::Results, 1) {
        record_limit(limiting_resources, QueryResource::Results)?;
        return Ok(());
    }
    let bytes = serialized_size(&candidate, u64::MAX, control)?;
    if !tracker.can_add(QueryResource::MemoryBytes, bytes) {
        record_limit(limiting_resources, QueryResource::MemoryBytes)?;
        return Ok(());
    }
    tracker.add_results(1)?;
    tracker.add_memory(bytes)?;
    try_push(candidates, candidate)?;
    Ok(())
}

fn occurrence_matches(occurrence: &rootlight_ir::OccurrenceRecord, symbol: SymbolId) -> bool {
    if occurrence.enclosing == Some(symbol) {
        return true;
    }
    match &occurrence.target {
        OccurrenceTarget::Resolved { symbol: target } => *target == symbol,
        OccurrenceTarget::Candidates { symbols, .. } => symbols.binary_search(&symbol).is_ok(),
        OccurrenceTarget::Unresolved { .. } => false,
    }
}

fn collect_coverage_partial(
    document: &NormalizedIrDocument,
    symbols: &BTreeSet<SymbolId>,
    files: &BTreeSet<FileId>,
    tracker: &mut UsageTracker,
    control: &QueryControl<'_>,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<Vec<CoverageRecord>, QueryError> {
    let mut coverage = Vec::new();
    for record in &document.coverage_records {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        let relevant = match record.scope {
            CoverageScope::Repository(repository) => repository == document.repository,
            CoverageScope::File(file) => files.contains(&file),
            CoverageScope::Entity(symbol) => symbols.contains(&symbol),
        };
        if relevant {
            if !tracker.can_add(QueryResource::Results, 1) {
                record_limit(limiting_resources, QueryResource::Results)?;
                break;
            }
            let bytes = serialized_size(record, u64::MAX, control)?;
            if !tracker.can_add(QueryResource::MemoryBytes, bytes) {
                record_limit(limiting_resources, QueryResource::MemoryBytes)?;
                break;
            }
            tracker.add_results(1)?;
            tracker.add_memory(bytes)?;
            try_push(&mut coverage, record.clone())?;
        }
    }
    Ok(coverage)
}

fn record_limit(
    limiting_resources: &mut Vec<QueryResource>,
    resource: QueryResource,
) -> Result<(), QueryError> {
    if !limiting_resources.contains(&resource) {
        try_push(limiting_resources, resource)?;
    }
    Ok(())
}

fn limits_optional_results(limiting_resources: &[QueryResource]) -> bool {
    limiting_resources.iter().any(|resource| {
        matches!(
            resource,
            QueryResource::Rows | QueryResource::Results | QueryResource::MemoryBytes
        )
    })
}

fn locate_hit_memory(hit: &rootlight_search::SearchHit) -> Result<u64, QueryError> {
    [
        hit.identifier.len(),
        hit.qualified_name.len(),
        hit.path.len(),
        hit.kind.len(),
        hit.language.len(),
        hit.tier.len(),
    ]
    .into_iter()
    .try_fold(
        u64::try_from(mem::size_of::<LocateHit>()).unwrap_or(u64::MAX),
        |total, length| {
            total
                .checked_add(checked_usize_to_u64(length)?)
                .ok_or(QueryError::MemoryUnavailable)
        },
    )
}

fn search_hit_text_bytes(hits: &[rootlight_search::SearchHit]) -> Result<u64, QueryError> {
    hits.iter().try_fold(0_u64, |total, hit| {
        [
            hit.identifier.len(),
            hit.qualified_name.len(),
            hit.path.len(),
            hit.kind.len(),
            hit.language.len(),
            hit.tier.len(),
        ]
        .into_iter()
        .try_fold(total, |subtotal, length| {
            subtotal
                .checked_add(checked_usize_to_u64(length)?)
                .ok_or(QueryError::MemoryUnavailable)
        })
    })
}

fn duration_micros(duration: Duration) -> u64 {
    checked_u128_to_u64(duration.as_nanos().saturating_add(999) / 1_000)
}

fn serialized_size(
    value: &impl Serialize,
    limit: u64,
    control: &QueryControl<'_>,
) -> Result<u64, QueryError> {
    let mut writer = CountingWriter::new(limit, control);
    if serde_json::to_writer(&mut writer, value).is_err() {
        return if let Some(reason) = writer.cancelled {
            Err(QueryError::Cancelled(reason))
        } else if writer.exceeded {
            Err(QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit,
            })
        } else {
            Err(QueryError::ResultEncoding)
        };
    }
    control.check()?;
    Ok(writer.count)
}

fn finish_response<T>(
    plan: PlanExplanation,
    data: T,
    tracker: UsageTracker,
    started: Instant,
    control: &QueryControl<'_>,
) -> Result<QueryResponse<T>, QueryError>
where
    T: Serialize,
{
    control.check()?;
    let elapsed_nanos = started.elapsed().as_nanos();
    let elapsed_micros = checked_u128_to_u64(elapsed_nanos.saturating_add(999) / 1_000);
    let mut response = QueryResponse {
        plan,
        data,
        usage: QueryUsage {
            rows: tracker.rows,
            edges: tracker.edges,
            results: tracker.results,
            source_bytes: tracker.source_bytes,
            json_bytes: 0,
            estimated_tokens: 0,
            token_accounting: TokenAccountingProfile::Utf8ByteUpperBoundV1,
            memory_bytes: tracker.memory_bytes,
            elapsed_micros,
        },
    };

    // The response contains its own byte and token counters. Re-encode until
    // their decimal widths reach a fixed point, then return the exact object
    // that was measured.
    for _ in 0..8 {
        let json_bytes =
            serialized_response_size(&response, tracker.budget.max_json_bytes, control)?;
        tracker.require(QueryResource::JsonBytes, json_bytes)?;
        let estimated_tokens = json_bytes;
        tracker.require(QueryResource::Tokens, estimated_tokens)?;
        if response.usage.json_bytes == json_bytes
            && response.usage.estimated_tokens == estimated_tokens
        {
            return Ok(response);
        }
        response.usage.json_bytes = json_bytes;
        response.usage.estimated_tokens = estimated_tokens;
    }
    Err(QueryError::ResultEncoding)
}

fn serialized_response_size(
    response: &impl Serialize,
    limit: u64,
    control: &QueryControl<'_>,
) -> Result<u64, QueryError> {
    serialized_size(response, limit, control).map_err(|error| {
        if matches!(
            error,
            QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                ..
            }
        ) {
            QueryError::BudgetExceeded {
                resource: QueryResource::JsonBytes,
                limit,
            }
        } else {
            error
        }
    })
}

struct UsageTracker {
    budget: QueryBudget,
    rows: u64,
    edges: u64,
    results: u64,
    source_bytes: u64,
    memory_bytes: u64,
}

impl UsageTracker {
    const fn new(budget: QueryBudget) -> Self {
        Self {
            budget,
            rows: 0,
            edges: 0,
            results: 0,
            source_bytes: 0,
            memory_bytes: 0,
        }
    }

    fn add_rows(&mut self, amount: u64) -> Result<(), QueryError> {
        self.rows = checked_add(self.rows, amount, QueryResource::Rows, self.budget.max_rows)?;
        Ok(())
    }

    fn add_edges(&mut self, amount: u64) -> Result<(), QueryError> {
        self.edges = checked_add(
            self.edges,
            amount,
            QueryResource::Edges,
            self.budget.max_edges,
        )?;
        Ok(())
    }

    fn add_results(&mut self, amount: u64) -> Result<(), QueryError> {
        self.results = checked_add(
            self.results,
            amount,
            QueryResource::Results,
            self.budget.max_results,
        )?;
        Ok(())
    }

    fn add_source_bytes(&mut self, amount: u64) -> Result<(), QueryError> {
        self.source_bytes = checked_add(
            self.source_bytes,
            amount,
            QueryResource::SourceBytes,
            self.budget.max_source_bytes,
        )?;
        Ok(())
    }

    fn add_memory(&mut self, amount: u64) -> Result<(), QueryError> {
        self.memory_bytes = checked_add(
            self.memory_bytes,
            amount,
            QueryResource::MemoryBytes,
            self.budget.max_memory_bytes,
        )?;
        Ok(())
    }

    fn require(&self, resource: QueryResource, value: u64) -> Result<(), QueryError> {
        let limit = match resource {
            QueryResource::Rows => self.budget.max_rows,
            QueryResource::Edges => self.budget.max_edges,
            QueryResource::Results => self.budget.max_results,
            QueryResource::SourceBytes => self.budget.max_source_bytes,
            QueryResource::JsonBytes => self.budget.max_json_bytes,
            QueryResource::Tokens => self.budget.max_tokens,
            QueryResource::MemoryBytes => self.budget.max_memory_bytes,
        };
        if value > limit {
            Err(QueryError::BudgetExceeded { resource, limit })
        } else {
            Ok(())
        }
    }

    fn can_add(&self, resource: QueryResource, amount: u64) -> bool {
        let (current, limit) = match resource {
            QueryResource::Rows => (self.rows, self.budget.max_rows),
            QueryResource::Edges => (self.edges, self.budget.max_edges),
            QueryResource::Results => (self.results, self.budget.max_results),
            QueryResource::SourceBytes => (self.source_bytes, self.budget.max_source_bytes),
            QueryResource::MemoryBytes => (self.memory_bytes, self.budget.max_memory_bytes),
            QueryResource::JsonBytes => (0, self.budget.max_json_bytes),
            QueryResource::Tokens => (0, self.budget.max_tokens),
        };
        current
            .checked_add(amount)
            .is_some_and(|value| value <= limit)
    }

    const fn remaining_memory(&self) -> u64 {
        self.budget
            .max_memory_bytes
            .saturating_sub(self.memory_bytes)
    }
}

struct QueryControl<'a> {
    cancellation: &'a Cancellation,
    deadline: Instant,
}

impl<'a> QueryControl<'a> {
    fn new(cancellation: &'a Cancellation, duration: Duration) -> Self {
        let started = Instant::now();
        Self {
            cancellation,
            deadline: started.checked_add(duration).unwrap_or(started),
        }
    }

    fn check(&self) -> Result<(), QueryError> {
        self.cancellation
            .check()
            .map_err(|cancelled| QueryError::Cancelled(cancelled.reason()))?;
        if Instant::now() >= self.deadline {
            return Err(QueryError::Cancelled(CancellationReason::DeadlineExceeded));
        }
        Ok(())
    }
}

struct CountingWriter<'control, 'cancellation> {
    count: u64,
    limit: u64,
    exceeded: bool,
    cancelled: Option<CancellationReason>,
    control: &'control QueryControl<'cancellation>,
}

impl<'control, 'cancellation> CountingWriter<'control, 'cancellation> {
    const fn new(limit: u64, control: &'control QueryControl<'cancellation>) -> Self {
        Self {
            count: 0,
            limit,
            exceeded: false,
            cancelled: None,
            control,
        }
    }
}

impl io::Write for CountingWriter<'_, '_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if let Err(QueryError::Cancelled(reason)) = self.control.check() {
            self.cancelled = Some(reason);
            return Err(io::Error::other("query output was cancelled"));
        }
        let amount = u64::try_from(buffer.len()).map_err(|_| {
            self.exceeded = true;
            io::Error::other("query output length is not representable")
        })?;
        self.count = self.count.checked_add(amount).ok_or_else(|| {
            self.exceeded = true;
            io::Error::other("query output length overflowed")
        })?;
        if self.count > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("query output exceeded its limit"));
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn try_reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<(), QueryError> {
    values
        .try_reserve(additional)
        .map_err(|_| QueryError::MemoryUnavailable)
}

fn try_push<T>(values: &mut Vec<T>, value: T) -> Result<(), QueryError> {
    if values.len() == values.capacity() {
        values
            .try_reserve(1)
            .map_err(|_| QueryError::MemoryUnavailable)?;
    }
    values.push(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Synthetic-graph proofs for the bounded `flow.trace` traversal.
    //!
    //! The first-slice oracle records calls as `DispatchCandidate` occurrences
    //! and containment as file-to-entity `Contains` relations, so no served
    //! relation family yields entity-to-entity edges for a lexical fixture.
    //! These tests exercise the traversal directly against hand-built adjacency
    //! views to prove path enumeration, targeting, cycle safety, and the depth
    //! and path caps independent of the oracle.

    use std::time::{Duration, Instant};

    use rootlight_cancel::Cancellation;
    use rootlight_ids::SymbolId;
    use rootlight_ir::RelationPredicate;

    use super::*;
    use crate::model::{FlowTraceFrontier, FlowTracePath, QueryBudget, RelationFamily};

    fn symbol(byte: u8) -> SymbolId {
        SymbolId::from_bytes([byte; 20])
    }

    fn edge(target: SymbolId, family: RelationFamily, confidence: u16) -> FlowAdjEdge {
        FlowAdjEdge {
            target,
            family,
            confidence,
            source_refs: Vec::new(),
        }
    }

    fn run_trace(
        adjacency: &BTreeMap<SymbolId, Vec<FlowAdjEdge>>,
        from: SymbolId,
        to: Option<SymbolId>,
        max_depth: u8,
        max_paths: usize,
    ) -> (Vec<FlowTracePath>, FlowTraceFrontier) {
        let budget = QueryBudget::new();
        let mut tracker = UsageTracker::new(budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, budget.max_duration);
        trace_flow(
            adjacency,
            from,
            to,
            max_depth,
            max_paths,
            &mut tracker,
            &mut limiting_resources,
            &control,
        )
        .expect("bounded trace succeeds")
    }

    #[test]
    fn flow_trace_enumerates_outward_paths_with_correct_nodes_and_edges() {
        let (a, b, c) = (symbol(1), symbol(2), symbol(3));
        let adjacency = BTreeMap::from([
            (a, vec![edge(b, RelationFamily::Calls, 900)]),
            (b, vec![edge(c, RelationFamily::Calls, 800)]),
        ]);
        let (paths, frontier) = run_trace(&adjacency, a, None, 3, 10);

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].nodes, vec![a, b]);
        assert_eq!(paths[0].edges.len(), 1);
        assert_eq!(paths[0].edges[0].family, RelationFamily::Calls);
        assert_eq!(paths[0].confidence, 900);
        assert!(!paths[0].cyclic);
        assert_eq!(paths[1].nodes, vec![a, b, c]);
        assert_eq!(paths[1].edges.len(), 2);
        // Aggregate confidence is the weakest link along the path.
        assert_eq!(paths[1].confidence, 800);

        assert_eq!(frontier.reached_nodes, 3);
        assert_eq!(frontier.examined_edges, 2);
        assert!(!frontier.truncated);
        assert_eq!(frontier.unresolved_boundaries, 0);
    }

    #[test]
    fn flow_trace_returns_only_paths_that_reach_the_target() {
        let (a, b, c) = (symbol(1), symbol(2), symbol(3));
        let adjacency = BTreeMap::from([
            (a, vec![edge(b, RelationFamily::Calls, 900)]),
            (b, vec![edge(c, RelationFamily::Calls, 800)]),
        ]);
        let (paths, _) = run_trace(&adjacency, a, Some(c), 3, 10);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].nodes, vec![a, b, c]);
        assert!(
            paths
                .iter()
                .all(|path| *path.nodes.last().expect("path has nodes") == c)
        );
    }

    #[test]
    fn flow_trace_marks_cycles_and_terminates() {
        let (a, b) = (symbol(1), symbol(2));
        let adjacency = BTreeMap::from([
            (a, vec![edge(b, RelationFamily::Calls, 500)]),
            (b, vec![edge(a, RelationFamily::Calls, 500)]),
        ]);
        let (paths, frontier) = run_trace(&adjacency, a, None, 8, 100);

        assert_eq!(paths.len(), 2);
        let cyclic = paths
            .iter()
            .find(|path| path.cyclic)
            .expect("one cyclic path");
        assert_eq!(cyclic.nodes, vec![a, b, a]);
        assert!(paths.iter().any(|path| !path.cyclic));
        assert_eq!(frontier.reached_nodes, 2);
        assert!(!frontier.truncated);
    }

    #[test]
    fn flow_trace_honors_the_depth_bound_and_reports_a_boundary() {
        let (a, b, c, d) = (symbol(1), symbol(2), symbol(3), symbol(4));
        let adjacency = BTreeMap::from([
            (a, vec![edge(b, RelationFamily::Calls, 900)]),
            (b, vec![edge(c, RelationFamily::Calls, 900)]),
            (c, vec![edge(d, RelationFamily::Calls, 900)]),
        ]);
        let (paths, frontier) = run_trace(&adjacency, a, None, 2, 100);

        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|path| path.nodes.len() <= 3));
        assert!(frontier.truncated);
        assert_eq!(frontier.reached_nodes, 3);
        assert_eq!(frontier.unresolved_boundaries, 1);
    }

    #[test]
    fn flow_trace_honors_the_path_cap() {
        let (a, b, c) = (symbol(1), symbol(2), symbol(3));
        let adjacency = BTreeMap::from([(
            a,
            vec![
                edge(b, RelationFamily::Calls, 900),
                edge(c, RelationFamily::Calls, 900),
            ],
        )]);
        let (paths, frontier) = run_trace(&adjacency, a, None, 3, 1);

        assert_eq!(paths.len(), 1);
        assert!(frontier.truncated);
    }

    #[test]
    fn predicate_family_picks_the_first_admitting_family_deterministically() {
        let ordered = vec![RelationFamily::Calls, RelationFamily::CalledBy];
        assert_eq!(
            predicate_family(&ordered, RelationPredicate::Calls),
            Some(RelationFamily::Calls)
        );
        assert_eq!(
            predicate_family(&[RelationFamily::CalledBy], RelationPredicate::Calls),
            Some(RelationFamily::CalledBy)
        );
        assert_eq!(
            predicate_family(&[RelationFamily::Imports], RelationPredicate::Calls),
            None
        );
    }

    // -----------------------------------------------------------------
    // architecture.cycles synthetic-graph proofs
    // -----------------------------------------------------------------

    use crate::model::{
        ArchitectureCyclesPlan, CycleBreak, CycleComponent, CyclePath, PlanEstimate,
        PlanExplanation, PlanKind,
    };
    use rootlight_ids::GenerationId;

    fn cycle_edge(target: SymbolId, confidence: u16) -> CycleAdjEdge {
        CycleAdjEdge {
            target,
            family: RelationFamily::Calls,
            confidence,
            source_refs: Vec::new(),
        }
    }

    fn cycle_plan(
        min_size: u8,
        max_cycles: usize,
        include_self_cycles: bool,
    ) -> ArchitectureCyclesPlan {
        ArchitectureCyclesPlan {
            families: vec![RelationFamily::Calls],
            min_confidence: 0,
            min_size,
            max_cycles,
            include_self_cycles,
            budget: QueryBudget::new(),
            explanation: PlanExplanation {
                generation: GenerationId::from_bytes([0; 20]),
                kind: PlanKind::ArchitectureCycles,
                operators: Vec::new(),
                estimate: PlanEstimate {
                    rows: 0,
                    edges: 0,
                    results: 0,
                    source_bytes: 0,
                    memory_bytes: 0,
                    json_bytes: 0,
                    estimated_tokens: 0,
                    duration_micros: 0,
                },
            },
        }
    }

    fn run_detect(
        adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
        min_size: u8,
        max_cycles: usize,
        include_self_cycles: bool,
    ) -> (Vec<CycleComponent>, Vec<CyclePath>, Vec<CycleBreak>) {
        let plan = cycle_plan(min_size, max_cycles, include_self_cycles);
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);
        detect_cycles(
            adjacency,
            &plan,
            &mut tracker,
            &mut limiting_resources,
            &control,
        )
        .expect("bounded cycle detection succeeds")
    }

    #[test]
    fn architecture_cycles_detects_a_two_cycle() {
        let (a, b) = (symbol(1), symbol(2));
        let adjacency =
            BTreeMap::from([(a, vec![cycle_edge(b, 900)]), (b, vec![cycle_edge(a, 700)])]);
        let (components, cycles, breaks) = run_detect(&adjacency, 2, 50, false);

        assert_eq!(components.len(), 1);
        assert_eq!(components[0].size, 2);
        assert_eq!(components[0].members, vec![a, b]);
        assert_eq!(components[0].internal_edges, 2);

        assert_eq!(cycles.len(), 1);
        // The cycle starts at the smallest member and repeats it at the end.
        assert_eq!(cycles[0].nodes, vec![a, b, a]);
        // Aggregate confidence is the weakest edge along the cycle.
        assert_eq!(cycles[0].confidence, 700);

        assert_eq!(breaks.len(), 1);
        // The cheapest break is the lowest-confidence edge (b -> a at 700).
        assert_eq!(breaks[0].from, b);
        assert_eq!(breaks[0].to, a);
        assert_eq!(breaks[0].break_cost, 700);
    }

    #[test]
    fn architecture_cycles_detects_a_three_cycle() {
        let (a, b, c) = (symbol(1), symbol(2), symbol(3));
        let adjacency = BTreeMap::from([
            (a, vec![cycle_edge(b, 900)]),
            (b, vec![cycle_edge(c, 800)]),
            (c, vec![cycle_edge(a, 600)]),
        ]);
        let (components, cycles, breaks) = run_detect(&adjacency, 2, 50, false);

        assert_eq!(components.len(), 1);
        assert_eq!(components[0].size, 3);
        assert_eq!(components[0].members, vec![a, b, c]);
        assert_eq!(components[0].internal_edges, 3);

        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].nodes, vec![a, b, c, a]);
        assert_eq!(cycles[0].confidence, 600);

        assert_eq!(breaks.len(), 1);
        assert_eq!(breaks[0].from, c);
        assert_eq!(breaks[0].to, a);
        assert_eq!(breaks[0].break_cost, 600);
    }

    #[test]
    fn architecture_cycles_handles_self_cycles_only_when_requested() {
        let a = symbol(1);
        let adjacency = BTreeMap::from([(a, vec![cycle_edge(a, 500)])]);

        let (components, cycles, breaks) = run_detect(&adjacency, 2, 50, false);
        assert!(components.is_empty());
        assert!(cycles.is_empty());
        assert!(breaks.is_empty());

        let (components, cycles, breaks) = run_detect(&adjacency, 2, 50, true);
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].size, 1);
        assert_eq!(components[0].members, vec![a]);
        assert_eq!(components[0].internal_edges, 1);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].nodes, vec![a, a]);
        assert_eq!(cycles[0].confidence, 500);
        assert_eq!(breaks.len(), 1);
        assert_eq!(breaks[0].from, a);
        assert_eq!(breaks[0].to, a);
    }

    #[test]
    fn architecture_cycles_honors_the_min_size_filter() {
        let (a, b, c, d) = (symbol(1), symbol(2), symbol(3), symbol(4));
        // One 2-cycle (a,b) and one 3-cycle (b,c,d) sharing no members would
        // overlap, so keep them disjoint: 2-cycle (a,b), 3-cycle (c,d plus a
        // third node) is awkward; use a clean 2-cycle and a separate 3-cycle.
        let e = symbol(5);
        let adjacency = BTreeMap::from([
            (a, vec![cycle_edge(b, 900)]),
            (b, vec![cycle_edge(a, 900)]),
            (c, vec![cycle_edge(d, 900)]),
            (d, vec![cycle_edge(e, 900)]),
            (e, vec![cycle_edge(c, 900)]),
        ]);

        let (components, _, _) = run_detect(&adjacency, 2, 50, false);
        assert_eq!(components.len(), 2);

        let (components, _, _) = run_detect(&adjacency, 3, 50, false);
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].size, 3);
        assert_eq!(components[0].members, vec![c, d, e]);
    }

    #[test]
    fn architecture_cycles_orders_components_deterministically() {
        let (a, b, c, d) = (symbol(1), symbol(2), symbol(3), symbol(4));
        // Two disjoint 2-cycles; larger-first then first-member ordering.
        let adjacency = BTreeMap::from([
            (c, vec![cycle_edge(d, 900)]),
            (d, vec![cycle_edge(c, 900)]),
            (a, vec![cycle_edge(b, 900)]),
            (b, vec![cycle_edge(a, 900)]),
        ]);
        let (first_components, first_cycles, first_breaks) = run_detect(&adjacency, 2, 50, false);
        let (second_components, second_cycles, second_breaks) =
            run_detect(&adjacency, 2, 50, false);

        assert_eq!(first_components, second_components);
        assert_eq!(first_cycles, second_cycles);
        assert_eq!(first_breaks, second_breaks);
        assert_eq!(first_components.len(), 2);
        // Equal sizes fall back to first-member order: (a,b) before (c,d).
        assert_eq!(first_components[0].members, vec![a, b]);
        assert_eq!(first_components[1].members, vec![c, d]);
    }

    #[test]
    fn architecture_cycles_reports_nothing_for_an_acyclic_graph() {
        let (a, b, c) = (symbol(1), symbol(2), symbol(3));
        let adjacency =
            BTreeMap::from([(a, vec![cycle_edge(b, 900)]), (b, vec![cycle_edge(c, 900)])]);
        let (components, cycles, breaks) = run_detect(&adjacency, 2, 50, true);
        assert!(components.is_empty());
        assert!(cycles.is_empty());
        assert!(breaks.is_empty());
    }

    #[test]
    fn architecture_cycles_honors_the_max_cycles_cap() {
        let (a, b, c, d) = (symbol(1), symbol(2), symbol(3), symbol(4));
        let adjacency = BTreeMap::from([
            (a, vec![cycle_edge(b, 900)]),
            (b, vec![cycle_edge(a, 900)]),
            (c, vec![cycle_edge(d, 900)]),
            (d, vec![cycle_edge(c, 900)]),
        ]);
        let (components, cycles, breaks) = run_detect(&adjacency, 2, 1, false);
        assert_eq!(components.len(), 1);
        assert_eq!(cycles.len(), 1);
        assert_eq!(breaks.len(), 1);
    }

    // -----------------------------------------------------------------
    // code.dead synthetic-graph proofs
    // -----------------------------------------------------------------

    use crate::model::{DeadCodeCandidate, DeadCodeClassification};
    use rootlight_ids::RepositoryId;
    use rootlight_ir::NormalizedIrDocument;

    /// Builds a directed dead-code graph from `(subject, object, confidence)`.
    fn dead_graph(edges: &[(SymbolId, SymbolId, u16)]) -> DeadGraph {
        let mut graph = DeadGraph::default();
        for &(subject, object, confidence) in edges {
            graph.nodes.insert(subject);
            graph.nodes.insert(object);
            graph
                .adjacency
                .entry(subject)
                .or_default()
                .push(DeadAdjEdge {
                    target: object,
                    confidence,
                });
            let count = graph.incoming_count.entry(object).or_insert(0);
            *count = count.saturating_add(1);
            let max_confidence = graph.incoming_max_confidence.entry(object).or_insert(0);
            if confidence > *max_confidence {
                *max_confidence = confidence;
            }
        }
        for outbound in graph.adjacency.values_mut() {
            outbound.sort_by(|left, right| {
                left.target
                    .cmp(&right.target)
                    .then_with(|| right.confidence.cmp(&left.confidence))
            });
        }
        graph
    }

    fn run_dead(
        graph: &DeadGraph,
        entry_points: &BTreeSet<SymbolId>,
        max_candidates: usize,
    ) -> Vec<DeadCodeCandidate> {
        let document = NormalizedIrDocument::empty(
            RepositoryId::from_bytes([0; 16]),
            GenerationId::from_bytes([0; 20]),
        );
        let exported = BTreeSet::new();
        let tests = BTreeSet::new();
        let budget = QueryBudget::new();
        let mut tracker = UsageTracker::new(budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, budget.max_duration);
        detect_dead_candidates(
            &document,
            graph,
            entry_points,
            &exported,
            &tests,
            max_candidates,
            &mut tracker,
            &mut limiting_resources,
            &control,
        )
        .expect("bounded dead-code detection succeeds")
    }

    #[test]
    fn code_dead_separates_reachable_from_unreachable_symbols() {
        let (entry, a, b, c, d) = (symbol(1), symbol(2), symbol(3), symbol(4), symbol(5));
        // entry -> a -> b is reachable; c -> d is an unreachable island.
        let graph = dead_graph(&[(entry, a, 900), (a, b, 900), (c, d, 900)]);
        let entry_points = BTreeSet::from([entry]);
        let candidates = run_dead(&graph, &entry_points, 50);

        let ids: Vec<SymbolId> = candidates.iter().map(|c| c.symbol_id).collect();
        assert_eq!(ids, vec![c, d]);
        assert_eq!(
            candidates[0].classification,
            DeadCodeClassification::ProvenDead
        );
        assert_eq!(
            candidates[1].classification,
            DeadCodeClassification::ProbableDead
        );
    }

    #[test]
    fn code_dead_marks_a_no_incoming_symbol_proven_dead() {
        let (entry, a, b, c) = (symbol(1), symbol(2), symbol(3), symbol(4));
        // entry -> a is reachable; b -> c is unreachable and b has no incoming.
        let graph = dead_graph(&[(entry, a, 900), (b, c, 900)]);
        let entry_points = BTreeSet::from([entry]);
        let candidates = run_dead(&graph, &entry_points, 50);

        let proven = candidates
            .iter()
            .find(|candidate| candidate.symbol_id == b)
            .expect("the no-incoming symbol is reported");
        assert_eq!(proven.classification, DeadCodeClassification::ProvenDead);
        assert_eq!(proven.confidence, 1_000);
        assert!(proven.why.contains(&"no_incoming_references".to_owned()));
        assert!(
            proven
                .why
                .contains(&"unreachable_from_entry_points".to_owned())
        );
    }

    #[test]
    fn code_dead_classifies_weak_incoming_edges_as_suspected() {
        let (entry, a, b, c) = (symbol(1), symbol(2), symbol(3), symbol(4));
        // entry -> a is reachable; b -> c is an unreachable island where c is
        // referenced only by a weak (low-confidence) edge from dead b.
        let graph = dead_graph(&[(entry, a, 900), (b, c, 100)]);
        let entry_points = BTreeSet::from([entry]);
        let candidates = run_dead(&graph, &entry_points, 50);

        let suspected = candidates
            .iter()
            .find(|candidate| candidate.symbol_id == c)
            .expect("the weakly referenced symbol is reported");
        assert_eq!(
            suspected.classification,
            DeadCodeClassification::SuspectedDead
        );
        assert_eq!(suspected.confidence, 400);
    }

    #[test]
    fn code_dead_excludes_entry_point_symbols_and_their_callees() {
        let (a, b, d, e) = (symbol(1), symbol(2), symbol(4), symbol(5));
        let graph = dead_graph(&[(a, b, 900), (d, e, 900)]);

        // With only `a` protected, the d -> e island is dead.
        let without = run_dead(&graph, &BTreeSet::from([a]), 50);
        let ids: Vec<SymbolId> = without.iter().map(|c| c.symbol_id).collect();
        assert_eq!(ids, vec![d, e]);

        // Protecting `d` as an entry point reaches both d and its callee e.
        let with = run_dead(&graph, &BTreeSet::from([a, d]), 50);
        assert!(with.is_empty());
    }

    #[test]
    fn code_dead_honors_the_max_candidates_cap() {
        let (a, b, c, d, e, f) = (
            symbol(1),
            symbol(2),
            symbol(3),
            symbol(4),
            symbol(5),
            symbol(6),
        );
        let graph = dead_graph(&[(a, b, 900), (c, d, 900), (e, f, 900)]);
        let entry_points = BTreeSet::from([a]);
        let candidates = run_dead(&graph, &entry_points, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].symbol_id, c);
    }

    #[test]
    fn code_dead_orders_candidates_deterministically() {
        let (a, b, c, d, e, f) = (
            symbol(1),
            symbol(2),
            symbol(3),
            symbol(4),
            symbol(5),
            symbol(6),
        );
        let graph = dead_graph(&[(e, f, 900), (c, d, 900), (a, b, 900)]);
        let entry_points = BTreeSet::from([a]);
        let first = run_dead(&graph, &entry_points, 50);
        let second = run_dead(&graph, &entry_points, 50);
        assert_eq!(first, second);
        let ids: Vec<SymbolId> = first.iter().map(|candidate| candidate.symbol_id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }
}
