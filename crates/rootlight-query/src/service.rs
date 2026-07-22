use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io, mem,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{FactId, FileId, GenerationId, SymbolId};
use rootlight_ir::{
    AnalysisTier, CoverageRecord, CoverageScope, CoverageStatus, EntityFlag, EntityKind,
    EntityVisibility, NormalizedIrDocument, OccurrenceTarget, RelationEndpoint, RelationPredicate,
    SourceRef,
};
use rootlight_search::{LexicalSearch, SearchBudget, SearchRequest, validate_search_request};
use rootlight_source::{
    SourceBudget, SourceEncoding as ServiceSourceEncoding, SourceError, SourceReadOptions,
    SourceService,
};
use rootlight_storage::GenerationSnapshot;
use serde::Serialize;

use crate::model::{
    ADVANCED_MAX_DEPTH, AdvancedAggregateFunction, AdvancedAstNode, AdvancedColumnSchema,
    AdvancedColumnType, AdvancedCompleteness, AdvancedEntityKind, AdvancedPlanExplanation,
    AdvancedPredicate, AdvancedQueryPlan, AdvancedQueryResult, AdvancedRelationKind,
    AdvancedSortKey, AdvancedTraverseDirection, AdvancedValue, ArchitectureComponent,
    ArchitectureConnection, ArchitectureCyclesPlan, ArchitectureCyclesProjection,
    ArchitectureCyclesResult, ArchitectureHotspot, ArchitectureOverviewDerivedView,
    ArchitectureOverviewPlan, ArchitectureOverviewResult, ArchitectureOverviewView,
    BreakingCandidateRecord, ChangeImpactClassification, ChangeImpactPlan, ChangeImpactResult,
    ChangeImpactRiskLevel, ChangeImpactRiskSummary, ChangeImpactTestCandidate, CodeDeadBlindSpot,
    CodeDeadEntryPointPolicy, CodeDeadEntryPointSummary, CodeDeadPlan, CodeDeadResult,
    CodeDeadSuppressionRule, CodeLocatePlan, CodeLocateResult, CycleBreak, CycleComponent,
    CyclePath, DeadCodeCandidate, DeadCodeClassification, ExecutionCompleteness, FlowTraceEdge,
    FlowTraceFrontier, FlowTracePath, FlowTracePlan, FlowTraceProjection, FlowTraceResult,
    HistoryArchitectureDelta, HistoryChangeKind, HistoryComparePlan, HistoryCompareResult,
    HistorySemanticChangeKind, ImpactEntryRecord, ImpactGroupRecord, LineageMatchRecord, LocateHit,
    LocateMode, PlanChangeContextPack, PlanChangeDecision, PlanChangeImpactSummary,
    PlanChangeObjective, PlanChangePlan, PlanChangeResult, PlanChangeStepRecord, PlanEstimate,
    PlanExplanation, PlanKind, QueryBudget, QueryError, QueryOperator, QueryResource,
    QueryResponse, QueryUsage, RankedTestSelection, RelationDirection, RelationFamily,
    RelationshipEdgeTarget, RelationshipGroup, RepositoryDataTrust, ResolvedChangeRecord,
    SemanticChangeRecord, SourceChunkEncoding, SourceChunkResult, SourceReadPlan,
    SourceReadQueryResult, SymbolExplainPlan, SymbolExplainResult, SymbolRelationshipsPlan,
    SymbolRelationshipsResult, TestsSelectCoverage, TestsSelectGap, TestsSelectKind,
    TestsSelectPlan, TestsSelectResult, TokenAccountingProfile, checked_add, checked_u128_to_u64,
    checked_usize_to_u64, ensure_estimate, search_mode,
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
        page_offset: usize,
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
            page_offset,
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
            page_offset,
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
            page_offset: plan.page_offset,
        };
        let outcome = self
            .search
            .search_with_stats(&request, plan.search_budget, cancellation)?;
        control.check()?;
        let returned_end = checked_add(
            checked_usize_to_u64(plan.page_offset)?,
            checked_usize_to_u64(outcome.hits.len())?,
            QueryResource::Results,
            u64::MAX,
        )?;
        if outcome.hits.len() > plan.max_results
            || outcome.matched_candidates < returned_end
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
        let has_more_matches = matched_candidates > returned_end;
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

        let (coverage, coverage_truncated) = collect_coverage_partial(
            self.generation.document(),
            &symbols,
            &files,
            &mut tracker,
            &control,
            &mut limiting_resources,
        )?;
        let next_page_offset = (has_more_matches && !coverage_truncated).then_some(returned_end);
        if next_page_offset.is_some() {
            record_limit(&mut limiting_resources, QueryResource::Results)?;
        }
        let execution = authoritative_execution(&limiting_resources);
        let data = CodeLocateResult {
            generation: self.generation.metadata().generation(),
            hits: located,
            matched_candidates,
            coverage,
            truncated: execution.is_truncated(),
            execution,
            limiting_resources,
            next_page_offset,
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
            .0
        };
        let execution = authoritative_execution(&limiting_resources);
        let data = SymbolExplainResult {
            generation: self.generation.metadata().generation(),
            entity: entity.clone(),
            relations,
            occurrences,
            provenance: provenance.clone(),
            coverage,
            truncated: execution.is_truncated(),
            execution,
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
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded relationships query dimension"
    )]
    pub fn plan_symbol_relationships(
        &self,
        seeds: BTreeSet<SymbolId>,
        families: Vec<RelationFamily>,
        direction: Option<RelationDirection>,
        min_confidence: u16,
        max_results: usize,
        page_offset: usize,
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
            page_offset,
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
        let page_start = checked_usize_to_u64(plan.page_offset)?;
        let page_end = checked_add(
            page_start,
            checked_usize_to_u64(plan.max_results)?,
            QueryResource::Results,
            u64::MAX,
        )?;

        let mut groups: BTreeMap<(SymbolId, RelationFamily, RelationDirection), RelationshipGroup> =
            BTreeMap::new();
        let mut total_edges: u64 = 0;
        let mut scan_truncated = false;

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
                    scan_truncated = true;
                    break 'scan;
                }
                if !tracker.can_add(QueryResource::Edges, 1) {
                    record_limit(&mut limiting_resources, QueryResource::Edges)?;
                    scan_truncated = true;
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
                    let bytes = serialized_size(relation, u64::MAX, &control)?;
                    if !tracker.can_add(QueryResource::MemoryBytes, bytes) {
                        record_limit(&mut limiting_resources, QueryResource::MemoryBytes)?;
                        scan_truncated = true;
                        break 'scan;
                    }
                    tracker.add_memory(bytes)?;
                    group.items.push(RelationshipEdgeTarget {
                        symbol: target,
                        confidence,
                        source_refs: relation.evidence.source.iter().cloned().collect(),
                    });
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
        let mut ordinal = 0_u64;
        let mut returned_edges = 0_u64;
        for group in &mut groups {
            let mut page_items = Vec::new();
            for item in group.items.drain(..) {
                if ordinal >= page_start && ordinal < page_end {
                    tracker.add_results(1)?;
                    page_items.push(item);
                    returned_edges = returned_edges.saturating_add(1);
                }
                ordinal = ordinal.saturating_add(1);
            }
            group.items = page_items;
        }
        groups.retain(|group| !group.items.is_empty());
        let next_page_offset = (!scan_truncated && total_edges > page_end).then_some(page_end);
        if next_page_offset.is_some() {
            record_limit(&mut limiting_resources, QueryResource::Results)?;
        }
        let execution = authoritative_execution(&limiting_resources);
        let truncated = execution.is_truncated();
        debug_assert_eq!(truncated, scan_truncated || next_page_offset.is_some());
        let data = SymbolRelationshipsResult {
            generation: self.generation.metadata().generation(),
            groups,
            returned_edges: u32::try_from(returned_edges).unwrap_or(u32::MAX),
            total_edges: u32::try_from(total_edges).unwrap_or(u32::MAX),
            exact: !scan_truncated,
            execution,
            truncated,
            limiting_resources,
            next_page_offset,
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
        let execution = authoritative_execution(&limiting_resources);
        debug_assert_eq!(frontier.truncated, execution.is_truncated());

        let data = FlowTraceResult {
            generation: self.generation.metadata().generation(),
            paths,
            frontier,
            projection: FlowTraceProjection {
                families: plan.families.clone(),
                min_confidence: plan.min_confidence,
            },
            execution,
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
        let execution = authoritative_execution(&limiting_resources);

        let data = ArchitectureCyclesResult {
            generation: self.generation.metadata().generation(),
            components,
            cycles,
            break_candidates,
            projection: ArchitectureCyclesProjection {
                families: plan.families.clone(),
                min_confidence: plan.min_confidence,
            },
            execution,
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
        let execution = authoritative_execution(&limiting_resources);

        let data = CodeDeadResult {
            generation: self.generation.metadata().generation(),
            candidates: analysis.candidates,
            entry_points: analysis.entry_points,
            blind_spots: analysis.blind_spots,
            suppression_rules: analysis.suppression_rules,
            execution,
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic bounded `architecture.overview` plan.
    ///
    /// A fixed served relation family set drives component-to-component
    /// connection aggregation, the requested views select derived-view
    /// metadata, and the confidence floor and component cap bound the
    /// aggregation.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, out-of-range confidence or
    /// component bounds, too many views, arithmetic overflow, or a conservative
    /// estimate that cannot be admitted.
    pub fn plan_architecture_overview(
        &self,
        mut views: Vec<ArchitectureOverviewView>,
        min_confidence: u16,
        max_components: usize,
        include_edges: bool,
        budget: QueryBudget,
    ) -> Result<ArchitectureOverviewPlan, QueryError> {
        budget.validate()?;
        if views.len() > 8 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if min_confidence > 1_000 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_components == 0
            || max_components > 250
            || checked_usize_to_u64(max_components)? > budget.max_results
        {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        views.sort();
        views.dedup();
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
            kind: PlanKind::ArchitectureOverview,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::RelationScan,
                QueryOperator::EntityLookup,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(ArchitectureOverviewPlan {
            views,
            min_confidence,
            max_components,
            include_edges,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `architecture.overview` plan.
    ///
    /// The scan groups symbols into file-granularity components from recorded
    /// containment and source evidence, aggregates served entity-level
    /// relations into typed component-to-component connections, and ranks
    /// components by structural fan-in and fan-out. Rows, edges, results, and
    /// memory are measured exactly like `architecture.cycles`.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, generation drift, encoding, or
    /// resource exhaustion.
    pub fn execute_architecture_overview(
        &self,
        plan: &ArchitectureOverviewPlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ArchitectureOverviewResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let document = self.generation.document();
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();

        let overview = build_architecture_overview(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )?;
        let execution = authoritative_execution(&limiting_resources);

        let data = ArchitectureOverviewResult {
            generation: self.generation.metadata().generation(),
            components: overview.components,
            connections: overview.connections,
            hotspots: overview.hotspots,
            views: overview.views,
            execution,
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic bounded `tests.select` plan.
    ///
    /// A non-empty seed set drives relevance ranking, the optional test-kind
    /// filter restricts the candidates, and the test cap bounds the ranking.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, an empty or oversized seed
    /// set, too many test kinds, out-of-range test bounds, arithmetic overflow,
    /// or a conservative estimate that cannot be admitted.
    pub fn plan_tests_select(
        &self,
        seeds: BTreeSet<SymbolId>,
        mut test_kinds: Vec<TestsSelectKind>,
        max_tests: usize,
        include_commands: bool,
        budget: QueryBudget,
    ) -> Result<TestsSelectPlan, QueryError> {
        budget.validate()?;
        if seeds.is_empty() || seeds.len() > 64 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if test_kinds.len() > 6 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_tests == 0
            || max_tests > 500
            || checked_usize_to_u64(max_tests)? > budget.max_results
        {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        test_kinds.sort();
        test_kinds.dedup();
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
            kind: PlanKind::TestsSelect,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::RelationScan,
                QueryOperator::EntityLookup,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(TestsSelectPlan {
            seeds,
            test_kinds,
            max_tests,
            include_commands,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `tests.select` plan.
    ///
    /// The scan identifies test entities, relates them to the seed set through
    /// served direct edges, bounded transitive paths, and file co-location,
    /// ranks them by a confidence-weighted signal score, and reports honest
    /// gaps for seeds with no related test. Rows, edges, results, and memory
    /// are measured exactly like `architecture.overview`.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, generation drift, encoding, or
    /// resource exhaustion.
    pub fn execute_tests_select(
        &self,
        plan: &TestsSelectPlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<TestsSelectResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let document = self.generation.document();
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();

        let selection = build_tests_select(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )?;
        let execution = authoritative_execution(&limiting_resources);

        let data = TestsSelectResult {
            generation: self.generation.metadata().generation(),
            tests: selection.tests,
            coverage_strategy: selection.coverage_strategy,
            gaps: selection.gaps,
            execution,
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic bounded `change.impact` plan.
    ///
    /// An explicit change set of stable symbols and repository-relative paths
    /// drives the analysis; the depth and confidence bounds and the dependent
    /// cap bound the transitive closure. Working-tree and revision-range diffs
    /// are not modeled here and must be rejected by the caller before planning.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, an empty or oversized
    /// change set, out-of-range depth, confidence, or dependent bounds,
    /// arithmetic overflow, or a conservative estimate that cannot be admitted.
    #[expect(
        clippy::too_many_arguments,
        reason = "the plan carries the explicit change set plus its bounded propagation options"
    )]
    pub fn plan_change_impact(
        &self,
        changed_symbols: BTreeSet<SymbolId>,
        mut changed_paths: Vec<String>,
        max_depth: u8,
        min_confidence: u16,
        include_tests: bool,
        max_dependents: usize,
        budget: QueryBudget,
    ) -> Result<ChangeImpactPlan, QueryError> {
        budget.validate()?;
        // The first slice maps only an explicit change set; an empty selector
        // carries no resolvable change.
        if changed_symbols.is_empty() && changed_paths.is_empty() {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if changed_symbols.len() > 256 || changed_paths.len() > 1_000 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_depth == 0 || max_depth > 8 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if min_confidence > 1_000 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_dependents == 0
            || max_dependents > 500
            || checked_usize_to_u64(max_dependents)? > budget.max_results
        {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        changed_paths.sort();
        changed_paths.dedup();
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
            kind: PlanKind::ChangeImpact,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::RelationScan,
                QueryOperator::EntityLookup,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(ChangeImpactPlan {
            changed_symbols,
            changed_paths,
            max_depth,
            min_confidence,
            include_tests,
            max_dependents,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `change.impact` plan.
    ///
    /// The scan resolves the explicit change set to symbols and files, builds a
    /// directed dependent graph over the served relation families, runs a
    /// bounded forward impact closure from each resolved change, optionally
    /// relates test entities to the impacted symbols, and aggregates an honest
    /// risk summary. Rows, edges, results, and memory are measured exactly like
    /// `tests.select`.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, generation drift, encoding, or
    /// resource exhaustion.
    pub fn execute_change_impact(
        &self,
        plan: &ChangeImpactPlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<ChangeImpactResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let document = self.generation.document();
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();

        let analysis = build_change_impact(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )?;
        let execution = authoritative_execution(&limiting_resources);

        let data = ChangeImpactResult {
            generation: self.generation.metadata().generation(),
            resolved_changes: analysis.resolved_changes,
            impacted: analysis.impacted,
            tests: analysis.tests,
            risk_summary: analysis.risk_summary,
            execution,
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic bounded `plan.change` plan.
    ///
    /// An explicit target set of stable symbols and files drives the analysis;
    /// the objective class colors the source-free step text, and the step cap
    /// bounds the emitted plan. The transitive closure reuses the change.impact
    /// depth and dependent bounds. Change context, user constraints, custom
    /// budgets, and non-compact profiles are not modeled here and must be
    /// rejected by the caller before planning.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, an empty or oversized
    /// target set, an out-of-range step cap, arithmetic overflow, or a
    /// conservative estimate that cannot be admitted.
    pub fn plan_plan_change(
        &self,
        objective: PlanChangeObjective,
        objective_text: String,
        target_symbols: BTreeSet<SymbolId>,
        target_files: BTreeSet<FileId>,
        max_steps: usize,
        budget: QueryBudget,
    ) -> Result<PlanChangePlan, QueryError> {
        budget.validate()?;
        // The first slice plans only an explicit target set; an empty selector
        // carries no resolvable target.
        if target_symbols.is_empty() && target_files.is_empty() {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if target_symbols.len() > 64 || target_files.len() > 64 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_steps == 0 || max_steps > 100 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if objective_text.is_empty() || objective_text.chars().count() > 4_096 {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        let max_dependents = PLAN_CHANGE_DEFAULT_DEPENDENTS
            .min(usize::try_from(budget.max_results).unwrap_or(usize::MAX));
        if max_dependents == 0 {
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
            kind: PlanKind::PlanChange,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::RelationScan,
                QueryOperator::EntityLookup,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(PlanChangePlan {
            objective,
            objective_text,
            target_symbols,
            target_files,
            max_steps,
            max_depth: PLAN_CHANGE_DEFAULT_DEPTH,
            max_dependents,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `plan.change` plan.
    ///
    /// The scan resolves the explicit targets to symbols, runs a bounded forward
    /// impact closure over the served relation families, relates test entities to
    /// the impacted symbols through the reused tests.select ranking, and builds a
    /// deterministic ordered plan with an honest impact summary, open decisions,
    /// and a ready context-pack request. Rows, edges, results, and memory are
    /// measured exactly like `change.impact`.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, generation drift, encoding, or
    /// resource exhaustion.
    pub fn execute_plan_change(
        &self,
        plan: &PlanChangePlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<PlanChangeResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let document = self.generation.document();
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();

        let analysis = build_plan_change(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )?;
        let execution = authoritative_execution(&limiting_resources);

        let data = PlanChangeResult {
            generation: self.generation.metadata().generation(),
            plan: analysis.plan,
            affected_scope: analysis.affected_scope,
            test_plan: analysis.test_plan,
            open_decisions: analysis.open_decisions,
            context_pack_request: analysis.context_pack_request,
            execution,
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic bounded `history.compare` plan.
    ///
    /// The plan pins the head generation to this service and carries the base
    /// generation identity explicitly. The optional change-kind filter and the
    /// result cap are validated here; scope bounding, unchanged-context
    /// inclusion, custom budgets, and non-compact profiles are not modeled and
    /// must be rejected by the caller before planning.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, an out-of-range result cap,
    /// an oversized change-kind filter, arithmetic overflow, or a conservative
    /// estimate that cannot be admitted.
    pub fn plan_history_compare(
        &self,
        base_generation: GenerationId,
        change_kinds: BTreeSet<HistoryChangeKind>,
        max_results: usize,
        budget: QueryBudget,
    ) -> Result<HistoryComparePlan, QueryError> {
        budget.validate()?;
        if max_results == 0 || max_results > HISTORY_COMPARE_MAX_RESULTS {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if change_kinds.len() > HISTORY_COMPARE_MAX_CHANGE_KINDS {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if checked_usize_to_u64(max_results)? > budget.max_results {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        let estimate = PlanEstimate {
            rows: budget.max_rows,
            edges: budget.max_edges,
            results: budget.max_results,
            source_bytes: 0,
            // Both normalized generations bound every record, while the query
            // memory budget remains the conservative aggregate ceiling.
            memory_bytes: budget.max_memory_bytes,
            json_bytes: budget.max_json_bytes,
            estimated_tokens: budget.max_tokens,
            duration_micros: duration_micros(budget.max_duration),
        };
        ensure_estimate(estimate, budget)?;
        let explanation = PlanExplanation {
            generation: self.generation.metadata().generation(),
            kind: PlanKind::HistoryCompare,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::EntityLookup,
                QueryOperator::RelationScan,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(HistoryComparePlan {
            base_generation,
            change_kinds,
            max_results,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `history.compare` plan.
    ///
    /// The head generation is this service's pinned generation; the caller
    /// supplies the resolved base generation document. The scan diffs the two
    /// entity sets by stable identity into added, removed, and modified changes,
    /// records identity-preserved lineage matches, ranks breaking public-surface
    /// removals and modifications by their base-generation consumer count, and
    /// reports an honest zero architecture delta. Rows, results, and memory are
    /// measured exactly like `change.impact`.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, generation drift, encoding, or
    /// resource exhaustion.
    pub fn execute_history_compare(
        &self,
        plan: &HistoryComparePlan,
        base_document: &NormalizedIrDocument,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<HistoryCompareResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let head_document = self.generation.document();
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();

        let analysis = build_history_compare(
            base_document,
            head_document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )?;
        let execution = authoritative_execution(&limiting_resources);

        let data = HistoryCompareResult {
            base_generation: plan.base_generation,
            head_generation: self.generation.metadata().generation(),
            coverage: analysis.coverage,
            changes: analysis.changes,
            architecture_delta: analysis.architecture_delta,
            breaking_candidates: analysis.breaking_candidates,
            lineage: analysis.lineage,
            execution,
            limiting_resources,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        };
        finish_response(plan.explanation.clone(), data, tracker, started, &control)
    }

    /// Builds a deterministic generation-bound `query.advanced` plan.
    ///
    /// The safe AST is walked to derive its operator sequence and nesting
    /// depth, validated against the resource ceilings, and admitted only when
    /// the static cost estimate fits both the hard ceiling and the optional
    /// client `cost_limit`. Execution serves an honest supported subset; the
    /// plan records whether an explanation was requested.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, an empty or too-deep AST,
    /// an out-of-range row or cumulative edge-work bound, a cost estimate that
    /// exceeds the ceiling or the client limit, or a conservative estimate that
    /// cannot be admitted.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded advanced query dimension"
    )]
    pub fn plan_advanced_query(
        &self,
        ast: AdvancedAstNode,
        explain: bool,
        max_results: usize,
        page_offset: usize,
        max_depth: usize,
        max_traversal: usize,
        cost_limit: Option<u64>,
        budget: QueryBudget,
    ) -> Result<AdvancedQueryPlan, QueryError> {
        budget.validate()?;
        if !advanced_traversal_depths_valid(&ast) {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Depth,
            });
        }
        let (operators, depth) = ast.derive_plan_shape();
        let estimated_cost =
            AdvancedQueryPlan::validate(&operators, max_results, max_traversal, depth)?;
        if depth > max_depth {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if !AdvancedQueryPlan::admits_cost(estimated_cost, cost_limit) {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if checked_usize_to_u64(max_results)? > budget.max_results {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        let estimate = PlanEstimate {
            rows: budget.max_rows,
            edges: budget.max_edges.min(checked_usize_to_u64(max_traversal)?),
            results: budget.max_results,
            source_bytes: 0,
            // The normalized generation bounds every inspected record while the
            // query memory budget remains the conservative aggregate ceiling.
            memory_bytes: budget.max_memory_bytes,
            json_bytes: budget.max_json_bytes,
            estimated_tokens: budget.max_tokens,
            duration_micros: duration_micros(budget.max_duration),
        };
        ensure_estimate(estimate, budget)?;
        let explanation = PlanExplanation {
            generation: self.generation.metadata().generation(),
            kind: PlanKind::QueryAdvanced,
            operators: vec![
                QueryOperator::GenerationPin,
                QueryOperator::EntityLookup,
                QueryOperator::OutputBudget,
            ],
            estimate,
        };
        Ok(AdvancedQueryPlan {
            ast,
            operators,
            max_rows: max_results,
            page_offset,
            max_traversal,
            depth,
            estimated_cost,
            explain,
            budget,
            explanation,
        })
    }

    /// Executes a prevalidated `query.advanced` plan.
    ///
    /// The supported operator subset (scan, filter, project, sort, limit) runs
    /// against the pinned generation's entities and returns typed rows keyed by
    /// column name. Unsupported patterns return honest non-empty columns with
    /// empty rows rather than fabricated data. When an explanation was
    /// requested, rows are empty and the plan is returned instead.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for cancellation, generation drift, encoding, or
    /// resource exhaustion.
    pub fn execute_advanced_query(
        &self,
        plan: &AdvancedQueryPlan,
        cancellation: &Cancellation,
    ) -> Result<QueryResponse<AdvancedQueryResult>, QueryError> {
        self.require_generation(plan.explanation.generation)?;
        let started = Instant::now();
        let control = QueryControl::new(cancellation, plan.budget.max_duration);
        control.check()?;
        let document = self.generation.document();
        let runtime_budget = advanced_runtime_budget(plan)?;
        let mut tracker = UsageTracker::new(runtime_budget);
        let mut limiting_resources = Vec::new();

        let built = build_advanced_query(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )?;

        let rows = if plan.explain { Vec::new() } else { built.rows };
        let data = AdvancedQueryResult {
            generation: self.generation.metadata().generation(),
            columns: built.columns,
            rows,
            plan: plan.explain.then_some(built.plan),
            execution: built.execution,
            completeness: built.completeness,
            limiting_resources,
            next_page_offset: built.next_page_offset,
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
            let encoding = match chunk.encoding {
                ServiceSourceEncoding::Utf8 => SourceChunkEncoding::Utf8,
                ServiceSourceEncoding::Bytes => SourceChunkEncoding::Bytes,
                _ => return Err(QueryError::InvalidSourceEncoding),
            };
            chunks.push(SourceChunkResult {
                reference: chunk.reference,
                path: chunk.path,
                start_byte: chunk.start_byte,
                end_byte: chunk.end_byte,
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                bytes: chunk.bytes,
                encoding,
                content_hash: chunk.content_hash,
                language: chunk.language,
                generated: chunk.generated,
                trust: RepositoryDataTrust::UntrustedRepositoryData,
            });
        }
        let data = SourceReadQueryResult {
            generation: result.generation,
            chunks,
            execution: ExecutionCompleteness::complete(),
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

/// Intermediate advanced query result before JSON encoding.
struct AdvancedBuild {
    columns: Vec<AdvancedColumnSchema>,
    rows: Vec<serde_json::Value>,
    plan: AdvancedPlanExplanation,
    execution: ExecutionCompleteness,
    completeness: AdvancedCompleteness,
    next_page_offset: Option<u64>,
}

/// A typed row set materialized during advanced query execution.
struct AdvancedRowSet {
    columns: Vec<AdvancedColumnSchema>,
    rows: Vec<BTreeMap<String, AdvancedValue>>,
}

fn advanced_runtime_budget(plan: &AdvancedQueryPlan) -> Result<QueryBudget, QueryError> {
    Ok(plan.budget.with_max_edges(
        plan.budget
            .max_edges
            .min(checked_usize_to_u64(plan.max_traversal)?),
    ))
}

/// Executes a bounded advanced query against the pinned generation document.
///
/// Supported operators are materialized into typed rows; unsupported traversal
/// patterns yield honest non-empty columns with empty rows. The plan
/// explanation is always derived so an `explain` request can return it without
/// materializing rows.
fn build_advanced_query(
    document: &NormalizedIrDocument,
    plan: &AdvancedQueryPlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<AdvancedBuild, QueryError> {
    let operator_names: Vec<String> = plan
        .operators
        .iter()
        .map(|operator| operator.as_str().to_owned())
        .collect();
    let explanation = AdvancedPlanExplanation {
        estimated_cost: plan.estimated_cost,
        operators: operator_names,
        applied_limits: vec![
            format!("rows<={}", plan.max_rows),
            format!("depth<={}", plan.depth),
            format!("traversal<={}", plan.max_traversal),
        ],
    };

    let supported = advanced_ast_supported(&plan.ast);
    let columns = advanced_derive_columns(&plan.ast);

    if plan.explain {
        let completeness = if supported {
            AdvancedCompleteness::Complete
        } else {
            note_advanced_limit(limiting_resources, QueryResource::Capability);
            AdvancedCompleteness::Unsupported
        };
        return Ok(AdvancedBuild {
            columns,
            rows: Vec::new(),
            plan: explanation,
            execution: if supported {
                ExecutionCompleteness::complete()
            } else {
                unsupported_execution(limiting_resources)
            },
            completeness,
            next_page_offset: None,
        });
    }

    if !supported {
        note_advanced_limit(limiting_resources, QueryResource::Capability);
        return Ok(AdvancedBuild {
            columns,
            rows: Vec::new(),
            plan: explanation,
            execution: unsupported_execution(limiting_resources),
            completeness: AdvancedCompleteness::Unsupported,
            next_page_offset: None,
        });
    }

    // Index file identities to presentation paths for scan rows.
    let mut file_paths: BTreeMap<FileId, String> = BTreeMap::new();
    for file in &document.files {
        file_paths.insert(file.id, file.path.clone());
    }

    let (mut set, truncated) =
        eval_advanced_node(document, &plan.ast, &file_paths, control, tracker)?;

    let page_start = plan.page_offset.min(set.rows.len());
    let page_end = page_start.saturating_add(plan.max_rows).min(set.rows.len());
    let next_page_offset = if !truncated && page_end < set.rows.len() {
        Some(checked_usize_to_u64(page_end)?)
    } else {
        None
    };
    let page_rows: Vec<BTreeMap<String, AdvancedValue>> =
        set.rows.drain(page_start..page_end).collect();

    let mut rows: Vec<serde_json::Value> = Vec::new();
    try_reserve(&mut rows, page_rows.len())?;
    for row in &page_rows {
        control.check()?;
        tracker.add_results(1)?;
        tracker.add_memory(checked_usize_to_u64(mem::size_of::<serde_json::Value>())?)?;
        rows.push(advanced_row_to_json(&set.columns, row));
    }

    if truncated || next_page_offset.is_some() {
        note_advanced_limit(limiting_resources, QueryResource::Rows);
    }
    let completeness = if truncated {
        AdvancedCompleteness::Truncated
    } else if next_page_offset.is_some() {
        AdvancedCompleteness::Paged
    } else {
        AdvancedCompleteness::Complete
    };
    let execution = match completeness {
        AdvancedCompleteness::Complete => ExecutionCompleteness::complete(),
        AdvancedCompleteness::Paged | AdvancedCompleteness::Truncated => {
            authoritative_execution(limiting_resources)
        }
        AdvancedCompleteness::Unsupported => unsupported_execution(limiting_resources),
    };

    Ok(AdvancedBuild {
        columns: set.columns,
        rows,
        plan: explanation,
        execution,
        completeness,
        next_page_offset,
    })
}

/// Whether an advanced AST can be served by the supported operator subset.
///
/// Traversal remains unavailable until its relation projection can be derived
/// from the pinned graph without weakening its declared edge and depth bounds.
fn advanced_ast_supported(node: &AdvancedAstNode) -> bool {
    match node {
        AdvancedAstNode::Scan { .. } => true,
        AdvancedAstNode::Filter { input, .. }
        | AdvancedAstNode::Project { input, .. }
        | AdvancedAstNode::Sort { input, .. }
        | AdvancedAstNode::Limit { input, .. } => advanced_ast_supported(input),
        AdvancedAstNode::Join { left, right, .. } => {
            advanced_ast_supported(left) && advanced_ast_supported(right)
        }
        AdvancedAstNode::Aggregate { input, .. } => advanced_ast_supported(input),
        AdvancedAstNode::Traverse {
            seed, seed_from, ..
        } => seed.is_some() && seed_from.is_none(),
    }
}

fn advanced_traversal_depths_valid(node: &AdvancedAstNode) -> bool {
    match node {
        AdvancedAstNode::Scan { .. } => true,
        AdvancedAstNode::Filter { input, .. }
        | AdvancedAstNode::Project { input, .. }
        | AdvancedAstNode::Aggregate { input, .. }
        | AdvancedAstNode::Sort { input, .. }
        | AdvancedAstNode::Limit { input, .. } => advanced_traversal_depths_valid(input),
        AdvancedAstNode::Join { left, right, .. } => {
            advanced_traversal_depths_valid(left) && advanced_traversal_depths_valid(right)
        }
        AdvancedAstNode::Traverse { max_depth, .. } => {
            max_depth.is_none_or(|depth| depth > 0 && usize::from(depth) <= ADVANCED_MAX_DEPTH)
        }
    }
}

/// Derives the non-empty output column schema for an advanced AST.
///
/// The schema is derived for both supported and unsupported patterns so the
/// contract's minimum-one-column invariant always holds.
fn advanced_derive_columns(node: &AdvancedAstNode) -> Vec<AdvancedColumnSchema> {
    match node {
        AdvancedAstNode::Scan { .. } => advanced_default_scan_columns(),
        AdvancedAstNode::Filter { input, .. }
        | AdvancedAstNode::Sort { input, .. }
        | AdvancedAstNode::Limit { input, .. } => advanced_derive_columns(input),
        AdvancedAstNode::Project { input, columns } => {
            let inner = advanced_derive_columns(input);
            advanced_project_schema(&inner, columns)
        }
        AdvancedAstNode::Join { left, right, .. } => {
            let left = advanced_derive_columns(left);
            let right = advanced_derive_columns(right);
            advanced_join_schema(&left, &right)
        }
        AdvancedAstNode::Aggregate {
            input,
            group_by,
            aggregations,
        } => {
            let input = advanced_derive_columns(input);
            let mut columns = Vec::new();
            for name in group_by {
                columns.push(AdvancedColumnSchema {
                    name: name.clone(),
                    column_type: advanced_column_type(&input, name),
                });
            }
            for aggregation in aggregations {
                columns.push(advanced_aggregate_column(aggregation, &input));
            }
            if columns.is_empty() {
                columns.push(advanced_default_id_column());
            }
            columns
        }
        AdvancedAstNode::Traverse { .. } => vec![
            AdvancedColumnSchema {
                name: "source".to_owned(),
                column_type: AdvancedColumnType::SymbolId,
            },
            AdvancedColumnSchema {
                name: "target".to_owned(),
                column_type: AdvancedColumnType::SymbolId,
            },
            AdvancedColumnSchema {
                name: "relation".to_owned(),
                column_type: AdvancedColumnType::Text,
            },
        ],
    }
}

/// Default columns produced by a scan over entities.
fn advanced_default_scan_columns() -> Vec<AdvancedColumnSchema> {
    vec![
        AdvancedColumnSchema {
            name: "id".to_owned(),
            column_type: AdvancedColumnType::SymbolId,
        },
        AdvancedColumnSchema {
            name: "kind".to_owned(),
            column_type: AdvancedColumnType::Text,
        },
        AdvancedColumnSchema {
            name: "name".to_owned(),
            column_type: AdvancedColumnType::Text,
        },
        AdvancedColumnSchema {
            name: "path".to_owned(),
            column_type: AdvancedColumnType::Path,
        },
    ]
}

/// Fallback single identity column guaranteeing a non-empty schema.
fn advanced_default_id_column() -> AdvancedColumnSchema {
    AdvancedColumnSchema {
        name: "id".to_owned(),
        column_type: AdvancedColumnType::SymbolId,
    }
}

/// Output column for one aggregate function.
fn advanced_aggregate_column(
    aggregation: &AdvancedAggregateFunction,
    input: &[AdvancedColumnSchema],
) -> AdvancedColumnSchema {
    match aggregation {
        AdvancedAggregateFunction::Count => AdvancedColumnSchema {
            name: "count".to_owned(),
            column_type: AdvancedColumnType::Integer,
        },
        AdvancedAggregateFunction::Sum { field } => AdvancedColumnSchema {
            name: format!("sum_{field}"),
            column_type: AdvancedColumnType::Integer,
        },
        AdvancedAggregateFunction::Min { field } => AdvancedColumnSchema {
            name: format!("min_{field}"),
            column_type: advanced_column_type(input, field),
        },
        AdvancedAggregateFunction::Max { field } => AdvancedColumnSchema {
            name: format!("max_{field}"),
            column_type: advanced_column_type(input, field),
        },
    }
}

fn advanced_column_type(columns: &[AdvancedColumnSchema], name: &str) -> AdvancedColumnType {
    columns
        .iter()
        .find(|column| column.name == name)
        .map_or(AdvancedColumnType::Text, |column| column.column_type)
}

fn advanced_join_schema(
    left: &[AdvancedColumnSchema],
    right: &[AdvancedColumnSchema],
) -> Vec<AdvancedColumnSchema> {
    let mut columns = left.to_vec();
    for column in right {
        if !columns.iter().any(|existing| existing.name == column.name) {
            columns.push(column.clone());
        }
    }
    columns
}

/// Projects an inner schema onto the requested column names.
///
/// Requested columns absent from the inner schema default to text so the
/// projected schema always mirrors the requested column list.
fn advanced_project_schema(
    inner: &[AdvancedColumnSchema],
    columns: &[String],
) -> Vec<AdvancedColumnSchema> {
    columns
        .iter()
        .map(|name| {
            let column_type = inner
                .iter()
                .find(|column| &column.name == name)
                .map(|column| column.column_type)
                .unwrap_or(AdvancedColumnType::Text);
            AdvancedColumnSchema {
                name: name.clone(),
                column_type,
            }
        })
        .collect()
}

/// Evaluates a supported advanced AST node into a typed row set.
///
/// Returns the row set and whether a limit already truncated the rows.
fn eval_advanced_node(
    document: &NormalizedIrDocument,
    node: &AdvancedAstNode,
    file_paths: &BTreeMap<FileId, String>,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
) -> Result<(AdvancedRowSet, bool), QueryError> {
    match node {
        AdvancedAstNode::Scan { entity, filter } => {
            let set = eval_advanced_scan(
                document,
                *entity,
                filter.as_deref(),
                file_paths,
                control,
                tracker,
            )?;
            Ok((set, false))
        }
        AdvancedAstNode::Filter { input, predicate } => {
            let (set, truncated) =
                eval_advanced_node(document, input, file_paths, control, tracker)?;
            Ok((advanced_filter_rows(set, predicate, control)?, truncated))
        }
        AdvancedAstNode::Project { input, columns } => {
            let (set, truncated) =
                eval_advanced_node(document, input, file_paths, control, tracker)?;
            Ok((advanced_project_rows(set, columns, control)?, truncated))
        }
        AdvancedAstNode::Sort { input, by } => {
            let (mut set, truncated) =
                eval_advanced_node(document, input, file_paths, control, tracker)?;
            advanced_sort_rows(&mut set.rows, by, control, tracker)?;
            Ok((set, truncated))
        }
        AdvancedAstNode::Limit { input, max_rows } => {
            let (set, truncated) =
                eval_advanced_node(document, input, file_paths, control, tracker)?;
            Ok((
                advanced_limit_rows(set, usize::from(*max_rows), control)?,
                truncated,
            ))
        }
        AdvancedAstNode::Join { left, right, on } => {
            let (left, left_truncated) =
                eval_advanced_node(document, left, file_paths, control, tracker)?;
            let (right, right_truncated) =
                eval_advanced_node(document, right, file_paths, control, tracker)?;
            let joined = advanced_join_rows(left, right, on, control, tracker)?;
            Ok((joined, left_truncated || right_truncated))
        }
        AdvancedAstNode::Aggregate {
            input,
            group_by,
            aggregations,
        } => {
            let (input, truncated) =
                eval_advanced_node(document, input, file_paths, control, tracker)?;
            let aggregated =
                advanced_aggregate_rows(input, group_by, aggregations, control, tracker)?;
            Ok((aggregated, truncated))
        }
        AdvancedAstNode::Traverse {
            seed,
            seed_from,
            relation,
            direction,
            max_depth,
        } => {
            let Some(seed) = *seed else {
                return Err(QueryError::PlanRejected {
                    resource: QueryResource::Results,
                });
            };
            if seed_from.is_some() {
                return Err(QueryError::PlanRejected {
                    resource: QueryResource::Results,
                });
            }
            let traversed = advanced_traverse_rows(
                document,
                seed,
                *relation,
                *direction,
                max_depth.unwrap_or(1),
                control,
                tracker,
            )?;
            Ok((traversed, false))
        }
    }
}

/// Scans entities of one kind, optionally filtering by a bounded predicate.
fn eval_advanced_scan(
    document: &NormalizedIrDocument,
    entity_kind: AdvancedEntityKind,
    filter: Option<&AdvancedPredicate>,
    file_paths: &BTreeMap<FileId, String>,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
) -> Result<AdvancedRowSet, QueryError> {
    let columns = advanced_default_scan_columns();
    let mut matched: Vec<&rootlight_ir::EntityRecord> = document
        .entities
        .iter()
        .filter(|entity| entity_kind.matches_ir(entity.kind))
        .collect();
    // Deterministic identity order independent of document insertion order.
    matched.sort_by_key(|entity| entity.id);
    let mut rows: Vec<BTreeMap<String, AdvancedValue>> = Vec::new();
    for entity in matched {
        control.check()?;
        tracker.add_rows(1)?;
        let row = advanced_scan_row(entity, file_paths)?;
        if filter.is_some_and(|predicate| !advanced_predicate_matches(predicate, &row)) {
            continue;
        }
        try_push(&mut rows, row)?;
    }
    Ok(AdvancedRowSet { columns, rows })
}

/// Builds one scan row keyed by the default scan columns.
fn advanced_scan_row(
    entity: &rootlight_ir::EntityRecord,
    file_paths: &BTreeMap<FileId, String>,
) -> Result<BTreeMap<String, AdvancedValue>, QueryError> {
    let mut row = BTreeMap::new();
    row.insert("id".to_owned(), AdvancedValue::Symbol(entity.id));
    row.insert(
        "kind".to_owned(),
        AdvancedValue::Text(serialized_label(&entity.kind)?),
    );
    row.insert(
        "name".to_owned(),
        AdvancedValue::Text(entity.canonical_name.clone()),
    );
    let path = entity
        .evidence
        .source
        .as_ref()
        .and_then(|source| file_paths.get(&source.span().file()).cloned())
        .unwrap_or_default();
    row.insert("path".to_owned(), AdvancedValue::Text(path));
    Ok(row)
}

/// Evaluates a bounded predicate against one row.
///
/// A predicate referencing a column absent from the row is not satisfied.
fn advanced_predicate_matches(
    predicate: &AdvancedPredicate,
    row: &BTreeMap<String, AdvancedValue>,
) -> bool {
    match predicate {
        AdvancedPredicate::Equals { field, value } => row.get(field) == Some(value),
        AdvancedPredicate::NotEquals { field, value } => {
            row.get(field).is_some_and(|current| current != value)
        }
        AdvancedPredicate::In { field, values } => row
            .get(field)
            .is_some_and(|current| values.contains(current)),
        AdvancedPredicate::And { predicates } => predicates
            .iter()
            .all(|inner| advanced_predicate_matches(inner, row)),
        AdvancedPredicate::Or { predicates } => predicates
            .iter()
            .any(|inner| advanced_predicate_matches(inner, row)),
    }
}

fn advanced_filter_rows(
    mut set: AdvancedRowSet,
    predicate: &AdvancedPredicate,
    control: &QueryControl<'_>,
) -> Result<AdvancedRowSet, QueryError> {
    let mut cancellation = None;
    set.rows.retain(|row| {
        if cancellation.is_some() {
            return false;
        }
        if let Err(error) = control.check() {
            cancellation = Some(error);
            return false;
        }
        advanced_predicate_matches(predicate, row)
    });
    if let Some(error) = cancellation {
        return Err(error);
    }
    Ok(set)
}

/// Projects each row onto the requested columns.
fn advanced_project_rows(
    set: AdvancedRowSet,
    columns: &[String],
    control: &QueryControl<'_>,
) -> Result<AdvancedRowSet, QueryError> {
    let schema = advanced_project_schema(&set.columns, columns);
    let mut rows = Vec::new();
    try_reserve(&mut rows, set.rows.len())?;
    for row in set.rows {
        control.check()?;
        let mut projected = BTreeMap::new();
        for name in columns {
            if let Some(value) = row.get(name) {
                projected.insert(name.clone(), value.clone());
            }
        }
        rows.push(projected);
    }
    Ok(AdvancedRowSet {
        columns: schema,
        rows,
    })
}

fn advanced_limit_rows(
    mut set: AdvancedRowSet,
    cap: usize,
    control: &QueryControl<'_>,
) -> Result<AdvancedRowSet, QueryError> {
    control.check()?;
    if set.rows.len() > cap {
        set.rows.truncate(cap);
    }
    Ok(set)
}

fn advanced_traverse_rows(
    document: &NormalizedIrDocument,
    seed: SymbolId,
    relation: AdvancedRelationKind,
    direction: AdvancedTraverseDirection,
    max_depth: u8,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
) -> Result<AdvancedRowSet, QueryError> {
    if max_depth == 0 || usize::from(max_depth) > ADVANCED_MAX_DEPTH {
        return Err(QueryError::PlanRejected {
            resource: QueryResource::Depth,
        });
    }
    let (predicate, invert_relation) = advanced_relation_projection(relation);
    let mut adjacency: BTreeMap<SymbolId, Vec<SymbolId>> = BTreeMap::new();
    for fact in &document.relations {
        control.check()?;
        tracker.add_edges(1)?;
        if fact.predicate != predicate {
            continue;
        }
        let (RelationEndpoint::Entity(subject), RelationEndpoint::Entity(object)) =
            (fact.subject, fact.object)
        else {
            continue;
        };
        let (source, target) = if invert_relation {
            (object, subject)
        } else {
            (subject, object)
        };
        match direction {
            AdvancedTraverseDirection::Outbound => {
                adjacency.entry(source).or_default().push(target);
            }
            AdvancedTraverseDirection::Inbound => {
                adjacency.entry(target).or_default().push(source);
            }
            AdvancedTraverseDirection::Both => {
                adjacency.entry(source).or_default().push(target);
                adjacency.entry(target).or_default().push(source);
            }
        }
    }
    for targets in adjacency.values_mut() {
        targets.sort_unstable();
        targets.dedup();
    }

    let mut frontier = BTreeSet::from([seed]);
    let mut visited = frontier.clone();
    let mut emitted = BTreeSet::new();
    let mut rows = Vec::new();
    for _ in 0..max_depth {
        let mut next = BTreeSet::new();
        for source in frontier {
            let Some(targets) = adjacency.get(&source) else {
                continue;
            };
            for target in targets {
                control.check()?;
                tracker.add_edges(1)?;
                if emitted.insert((source, *target)) {
                    let mut row = BTreeMap::new();
                    row.insert("source".to_owned(), AdvancedValue::Symbol(source));
                    row.insert("target".to_owned(), AdvancedValue::Symbol(*target));
                    row.insert(
                        "relation".to_owned(),
                        AdvancedValue::Text(relation.as_str().to_owned()),
                    );
                    try_push(&mut rows, row)?;
                }
                if visited.insert(*target) {
                    next.insert(*target);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(AdvancedRowSet {
        columns: advanced_derive_columns(&AdvancedAstNode::Traverse {
            seed: Some(seed),
            seed_from: None,
            relation,
            direction,
            max_depth: Some(max_depth),
        }),
        rows,
    })
}

const fn advanced_relation_projection(relation: AdvancedRelationKind) -> (RelationPredicate, bool) {
    match relation {
        AdvancedRelationKind::Calls => (RelationPredicate::Calls, false),
        AdvancedRelationKind::CalledBy => (RelationPredicate::Calls, true),
        AdvancedRelationKind::Imports => (RelationPredicate::Imports, false),
        AdvancedRelationKind::ImportedBy => (RelationPredicate::Imports, true),
        AdvancedRelationKind::Tests => (RelationPredicate::Tests, false),
        AdvancedRelationKind::TestedBy => (RelationPredicate::Tests, true),
        AdvancedRelationKind::Contains => (RelationPredicate::Contains, false),
        AdvancedRelationKind::ContainedBy => (RelationPredicate::Contains, true),
        AdvancedRelationKind::Implements => (RelationPredicate::Implements, false),
        AdvancedRelationKind::ImplementedBy => (RelationPredicate::Implements, true),
        AdvancedRelationKind::References => (RelationPredicate::RefersTo, false),
        AdvancedRelationKind::ReferencedBy => (RelationPredicate::RefersTo, true),
    }
}

const ADVANCED_BTREE_ENTRY_OVERHEAD_BYTES: usize = mem::size_of::<usize>() * 4;

fn advanced_value_dynamic_bytes(value: &AdvancedValue) -> usize {
    match value {
        AdvancedValue::Text(text) => text.len(),
        AdvancedValue::Integer(_)
        | AdvancedValue::Boolean(_)
        | AdvancedValue::Symbol(_)
        | AdvancedValue::File(_) => 0,
    }
}

fn advanced_row_owned_bytes(row: &BTreeMap<String, AdvancedValue>) -> Result<u64, QueryError> {
    let mut bytes = mem::size_of::<BTreeMap<String, AdvancedValue>>();
    for (name, value) in row {
        bytes = bytes
            .saturating_add(ADVANCED_BTREE_ENTRY_OVERHEAD_BYTES)
            .saturating_add(mem::size_of::<String>())
            .saturating_add(name.len())
            .saturating_add(mem::size_of::<AdvancedValue>())
            .saturating_add(advanced_value_dynamic_bytes(value));
    }
    checked_usize_to_u64(bytes)
}

fn advanced_group_owned_bytes(
    key: &[AdvancedValue],
    aggregate_count: usize,
) -> Result<u64, QueryError> {
    let mut bytes = mem::size_of::<BTreeMap<Vec<AdvancedValue>, Vec<AdvancedAggregateState>>>()
        .saturating_add(ADVANCED_BTREE_ENTRY_OVERHEAD_BYTES)
        .saturating_add(mem::size_of::<Vec<AdvancedValue>>())
        .saturating_add(key.len().saturating_mul(mem::size_of::<AdvancedValue>()))
        .saturating_add(mem::size_of::<Vec<AdvancedAggregateState>>())
        .saturating_add(aggregate_count.saturating_mul(mem::size_of::<AdvancedAggregateState>()));
    for value in key {
        bytes = bytes.saturating_add(advanced_value_dynamic_bytes(value));
    }
    checked_usize_to_u64(bytes)
}

fn advanced_join_rows(
    left: AdvancedRowSet,
    right: AdvancedRowSet,
    on: &str,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
) -> Result<AdvancedRowSet, QueryError> {
    let left_key = left.columns.iter().find(|column| column.name == on);
    let right_key = right.columns.iter().find(|column| column.name == on);
    if left_key
        .zip(right_key)
        .is_none_or(|(left, right)| left.column_type != right.column_type)
    {
        return Err(QueryError::PlanRejected {
            resource: QueryResource::Results,
        });
    }

    let columns = advanced_join_schema(&left.columns, &right.columns);
    let mut rows = Vec::new();
    for left_row in &left.rows {
        let Some(left_value) = left_row.get(on) else {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        };
        for right_row in &right.rows {
            control.check()?;
            tracker.add_edges(1)?;
            if right_row.get(on) != Some(left_value) {
                continue;
            }
            let mut joined = left_row.clone();
            for (name, value) in right_row {
                if let Some(existing) = joined.get(name) {
                    if existing != value {
                        return Err(QueryError::PlanRejected {
                            resource: QueryResource::Results,
                        });
                    }
                } else {
                    joined.insert(name.clone(), value.clone());
                }
            }
            tracker.add_memory(advanced_row_owned_bytes(&joined)?)?;
            try_push(&mut rows, joined)?;
        }
    }
    Ok(AdvancedRowSet { columns, rows })
}

enum AdvancedAggregateState {
    Count(i64),
    Sum(i64),
    Min(Option<AdvancedValue>),
    Max(Option<AdvancedValue>),
}

fn advanced_aggregate_rows(
    input: AdvancedRowSet,
    group_by: &[String],
    aggregations: &[AdvancedAggregateFunction],
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
) -> Result<AdvancedRowSet, QueryError> {
    if group_by
        .iter()
        .chain(
            aggregations
                .iter()
                .filter_map(|aggregation| match aggregation {
                    AdvancedAggregateFunction::Count => None,
                    AdvancedAggregateFunction::Sum { field }
                    | AdvancedAggregateFunction::Min { field }
                    | AdvancedAggregateFunction::Max { field } => Some(field),
                }),
        )
        .any(|name| !input.columns.iter().any(|column| &column.name == name))
    {
        return Err(QueryError::PlanRejected {
            resource: QueryResource::Results,
        });
    }

    let columns = {
        let mut columns = Vec::new();
        try_reserve(
            &mut columns,
            group_by.len().saturating_add(aggregations.len()),
        )?;
        for name in group_by {
            columns.push(AdvancedColumnSchema {
                name: name.clone(),
                column_type: advanced_column_type(&input.columns, name),
            });
        }
        for aggregation in aggregations {
            columns.push(advanced_aggregate_column(aggregation, &input.columns));
        }
        columns
    };
    let mut groups: BTreeMap<Vec<AdvancedValue>, Vec<AdvancedAggregateState>> = BTreeMap::new();
    for row in &input.rows {
        control.check()?;
        tracker.add_edges(1)?;
        let mut key = Vec::new();
        try_reserve(&mut key, group_by.len())?;
        for name in group_by {
            let Some(value) = row.get(name) else {
                return Err(QueryError::PlanRejected {
                    resource: QueryResource::Results,
                });
            };
            key.push(value.clone());
        }
        if !groups.contains_key(&key) {
            tracker.add_memory(advanced_group_owned_bytes(&key, aggregations.len())?)?;
            groups.insert(key.clone(), initial_aggregate_states(aggregations)?);
        }
        let states = groups.get_mut(&key).ok_or(QueryError::PlanRejected {
            resource: QueryResource::Results,
        })?;
        update_aggregate_states(states, aggregations, row, tracker)?;
    }

    let mut rows = Vec::new();
    try_reserve(&mut rows, groups.len())?;
    for (key, states) in groups {
        let mut row = BTreeMap::new();
        for (name, value) in group_by.iter().zip(key) {
            row.insert(name.clone(), value);
        }
        for (column, state) in columns.iter().skip(group_by.len()).zip(states) {
            row.insert(column.name.clone(), aggregate_state_value(state)?);
        }
        tracker.add_memory(advanced_row_owned_bytes(&row)?)?;
        rows.push(row);
    }
    Ok(AdvancedRowSet { columns, rows })
}

fn initial_aggregate_states(
    aggregations: &[AdvancedAggregateFunction],
) -> Result<Vec<AdvancedAggregateState>, QueryError> {
    let mut states = Vec::new();
    try_reserve(&mut states, aggregations.len())?;
    for aggregation in aggregations {
        states.push(match aggregation {
            AdvancedAggregateFunction::Count => AdvancedAggregateState::Count(0),
            AdvancedAggregateFunction::Sum { .. } => AdvancedAggregateState::Sum(0),
            AdvancedAggregateFunction::Min { .. } => AdvancedAggregateState::Min(None),
            AdvancedAggregateFunction::Max { .. } => AdvancedAggregateState::Max(None),
        });
    }
    Ok(states)
}

fn update_aggregate_states(
    states: &mut [AdvancedAggregateState],
    aggregations: &[AdvancedAggregateFunction],
    row: &BTreeMap<String, AdvancedValue>,
    tracker: &mut UsageTracker,
) -> Result<(), QueryError> {
    for (state, aggregation) in states.iter_mut().zip(aggregations) {
        match (state, aggregation) {
            (AdvancedAggregateState::Count(count), AdvancedAggregateFunction::Count) => {
                *count = count.checked_add(1).ok_or(QueryError::PlanRejected {
                    resource: QueryResource::Results,
                })?;
            }
            (AdvancedAggregateState::Sum(total), AdvancedAggregateFunction::Sum { field }) => {
                let Some(AdvancedValue::Integer(value)) = row.get(field) else {
                    return Err(QueryError::PlanRejected {
                        resource: QueryResource::Results,
                    });
                };
                *total = total.checked_add(*value).ok_or(QueryError::PlanRejected {
                    resource: QueryResource::Results,
                })?;
            }
            (AdvancedAggregateState::Min(current), AdvancedAggregateFunction::Min { field }) => {
                let Some(value) = row.get(field) else {
                    return Err(QueryError::PlanRejected {
                        resource: QueryResource::Results,
                    });
                };
                if current.as_ref().is_none_or(|minimum| value < minimum) {
                    tracker
                        .add_memory(checked_usize_to_u64(advanced_value_dynamic_bytes(value))?)?;
                    *current = Some(value.clone());
                }
            }
            (AdvancedAggregateState::Max(current), AdvancedAggregateFunction::Max { field }) => {
                let Some(value) = row.get(field) else {
                    return Err(QueryError::PlanRejected {
                        resource: QueryResource::Results,
                    });
                };
                if current.as_ref().is_none_or(|maximum| value > maximum) {
                    tracker
                        .add_memory(checked_usize_to_u64(advanced_value_dynamic_bytes(value))?)?;
                    *current = Some(value.clone());
                }
            }
            _ => {
                return Err(QueryError::PlanRejected {
                    resource: QueryResource::Results,
                });
            }
        }
    }
    Ok(())
}

fn aggregate_state_value(state: AdvancedAggregateState) -> Result<AdvancedValue, QueryError> {
    match state {
        AdvancedAggregateState::Count(value) | AdvancedAggregateState::Sum(value) => {
            Ok(AdvancedValue::Integer(value))
        }
        AdvancedAggregateState::Min(value) | AdvancedAggregateState::Max(value) => {
            value.ok_or(QueryError::PlanRejected {
                resource: QueryResource::Results,
            })
        }
    }
}

/// Sorts rows deterministically by the requested keys with a stable tie-break.
fn advanced_sort_rows(
    rows: &mut Vec<BTreeMap<String, AdvancedValue>>,
    by: &[AdvancedSortKey],
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
) -> Result<(), QueryError> {
    control.check()?;
    if rows.len() < 2 {
        return Ok(());
    }

    tracker.add_memory(advanced_sort_workspace_bytes(rows.len())?)?;

    let mut order = Vec::new();
    try_reserve(&mut order, rows.len())?;
    order.extend(0..rows.len());
    let mut scratch = Vec::new();
    try_reserve(&mut scratch, rows.len())?;
    scratch.resize(rows.len(), 0);

    let mut width = 1_usize;
    while width < order.len() {
        let span = width.saturating_mul(2);
        let mut start = 0_usize;
        while start < order.len() {
            let middle = start.saturating_add(width).min(order.len());
            let end = start.saturating_add(span).min(order.len());
            let (mut left, mut right, mut output) = (start, middle, start);
            while left < middle && right < end {
                control.check()?;
                if advanced_compare_rows(&rows[order[left]], &rows[order[right]], by)
                    != std::cmp::Ordering::Greater
                {
                    scratch[output] = order[left];
                    left += 1;
                } else {
                    scratch[output] = order[right];
                    right += 1;
                }
                output += 1;
            }
            while left < middle {
                control.check()?;
                scratch[output] = order[left];
                left += 1;
                output += 1;
            }
            while right < end {
                control.check()?;
                scratch[output] = order[right];
                right += 1;
                output += 1;
            }
            start = end;
        }
        std::mem::swap(&mut order, &mut scratch);
        width = span;
    }

    let mut slots = Vec::new();
    try_reserve(&mut slots, rows.len())?;
    slots.extend(std::mem::take(rows).into_iter().map(Some));
    try_reserve(rows, slots.len())?;
    for index in order {
        control.check()?;
        let row = slots[index].take().ok_or(QueryError::PlanRejected {
            resource: QueryResource::Results,
        })?;
        rows.push(row);
    }
    Ok(())
}

fn advanced_sort_workspace_bytes(row_count: usize) -> Result<u64, QueryError> {
    let index_bytes = row_count
        .saturating_mul(mem::size_of::<usize>())
        .saturating_mul(2);
    let row_slots = row_count
        .saturating_mul(mem::size_of::<Option<BTreeMap<String, AdvancedValue>>>())
        .saturating_mul(2);
    checked_usize_to_u64(index_bytes.saturating_add(row_slots))
}

fn advanced_compare_rows(
    left: &BTreeMap<String, AdvancedValue>,
    right: &BTreeMap<String, AdvancedValue>,
    by: &[AdvancedSortKey],
) -> std::cmp::Ordering {
    for key in by {
        let ordering = match (left.get(&key.field), right.get(&key.field)) {
            (Some(left_value), Some(right_value)) => left_value.cmp(right_value),
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (None, None) => std::cmp::Ordering::Equal,
        };
        let ordering = if key.descending {
            ordering.reverse()
        } else {
            ordering
        };
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    // Total deterministic tie-break over the ordered row contents.
    left.cmp(right)
}

/// Encodes one intermediate row as a JSON object keyed by column name.
fn advanced_row_to_json(
    columns: &[AdvancedColumnSchema],
    row: &BTreeMap<String, AdvancedValue>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for column in columns {
        let value = row
            .get(&column.name)
            .map(advanced_value_to_json)
            .unwrap_or(serde_json::Value::Null);
        map.insert(column.name.clone(), value);
    }
    serde_json::Value::Object(map)
}

/// Encodes one typed value as JSON.
fn advanced_value_to_json(value: &AdvancedValue) -> serde_json::Value {
    match value {
        AdvancedValue::Text(text) => serde_json::Value::String(text.clone()),
        AdvancedValue::Integer(integer) => serde_json::Value::Number((*integer).into()),
        AdvancedValue::Boolean(boolean) => serde_json::Value::Bool(*boolean),
        AdvancedValue::Symbol(symbol) => serde_json::Value::String(symbol.to_string()),
        AdvancedValue::File(file) => serde_json::Value::String(file.to_string()),
    }
}

/// Records a limiting resource once, preserving deterministic execution order.
fn note_advanced_limit(limiting_resources: &mut Vec<QueryResource>, resource: QueryResource) {
    if !limiting_resources.contains(&resource) {
        limiting_resources.push(resource);
    }
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
            record_limit(state.limiting_resources, QueryResource::Depth)?;
            state.depth_cut = true;
        }
        return Ok(());
    }

    let Some(neighbors) = adjacency.get(&node) else {
        return Ok(());
    };
    for edge in neighbors {
        if state.paths.len() >= max_paths {
            record_limit(state.limiting_resources, QueryResource::Paths)?;
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

/// Served relation families aggregated into architecture connections.
///
/// Each family maps to a disjoint IR predicate set, so a served relation
/// contributes to exactly one connection kind. `CalledBy` is intentionally
/// omitted because it shares the `Calls` predicate and would double-count the
/// same directed edge.
const ARCHITECTURE_OVERVIEW_FAMILIES: &[RelationFamily] = &[
    RelationFamily::Calls,
    RelationFamily::References,
    RelationFamily::Types,
    RelationFamily::Implements,
    RelationFamily::Imports,
];

/// Aggregated architecture overview assembled before bounded result emission.
struct ArchitectureOverviewAnalysis {
    components: Vec<ArchitectureComponent>,
    connections: Vec<ArchitectureConnection>,
    hotspots: Vec<ArchitectureHotspot>,
    views: Vec<ArchitectureOverviewDerivedView>,
}

/// Returns the stable algorithm-version label for one derived view.
const fn architecture_overview_algorithm_version(view: ArchitectureOverviewView) -> &'static str {
    match view {
        ArchitectureOverviewView::Hotspots => "fan_in_out_v1",
    }
}

/// Returns the repository-controlled display path for one file, falling back to
/// the stable file identity when the file record is not served.
fn architecture_file_name(document: &NormalizedIrDocument, file: FileId) -> String {
    find_file(document, file)
        .map(|record| record.path.clone())
        .unwrap_or_else(|| file.to_string())
}

/// Builds a bounded file-granularity architecture overview.
///
/// Symbols are grouped into one component per containing file, resolved from
/// recorded `Contains` relations with a source-evidence fallback. Served
/// entity-level relations are aggregated into typed connections between
/// distinct components, and components are ranked by structural fan-in and
/// fan-out. The component list is capped deterministically by symbol count and
/// identity; connections and hotspots reference only reported components.
fn build_architecture_overview(
    document: &NormalizedIrDocument,
    plan: &ArchitectureOverviewPlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<ArchitectureOverviewAnalysis, QueryError> {
    // Assign each entity to its declaring file from immutable source evidence
    // and record its closed kind label for responsibility evidence.
    let mut entity_evidence_file: BTreeMap<SymbolId, FileId> = BTreeMap::new();
    let mut entity_kind: BTreeMap<SymbolId, String> = BTreeMap::new();
    for entity in &document.entities {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if let Some(source) = entity.evidence.source.as_ref() {
            entity_evidence_file.insert(entity.id, source.span().file());
        }
        entity_kind.insert(entity.id, serialized_label(&entity.kind)?);
    }

    // Single bounded relation scan: `Contains` relations confirm the owning
    // file of each entity and supply containment confidence, while served
    // family relations contribute raw entity-to-entity edges for aggregation.
    let allowed: BTreeSet<RelationPredicate> = ARCHITECTURE_OVERVIEW_FAMILIES
        .iter()
        .flat_map(|family| family.predicates().iter().copied())
        .collect();
    let mut entity_contains_file: BTreeMap<SymbolId, FileId> = BTreeMap::new();
    let mut file_confidence: BTreeMap<FileId, u16> = BTreeMap::new();
    let mut raw_edges: Vec<(SymbolId, SymbolId, RelationFamily, u16)> = Vec::new();
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if relation.predicate == RelationPredicate::Contains {
            if let (RelationEndpoint::File(file), RelationEndpoint::Entity(symbol)) =
                (relation.subject, relation.object)
            {
                entity_contains_file.insert(symbol, file);
                let confidence = relation.confidence.get();
                let slot = file_confidence.entry(file).or_insert(0);
                if confidence > *slot {
                    *slot = confidence;
                }
            }
            continue;
        }
        if !plan.include_edges || !allowed.contains(&relation.predicate) {
            continue;
        }
        let confidence = relation.confidence.get();
        if confidence < plan.min_confidence {
            continue;
        }
        let Some(family) = predicate_family(ARCHITECTURE_OVERVIEW_FAMILIES, relation.predicate)
        else {
            continue;
        };
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        if subject == object {
            continue;
        }
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            break;
        }
        tracker.add_edges(1)?;
        try_push(&mut raw_edges, (subject, object, family, confidence))?;
    }

    // Resolve the authoritative entity-to-file assignment, preferring an
    // explicit `Contains` relation and falling back to source evidence.
    let mut entity_file: BTreeMap<SymbolId, FileId> = entity_evidence_file;
    for (symbol, file) in entity_contains_file {
        entity_file.insert(symbol, file);
    }

    // Group symbols into file-granularity components.
    let mut file_members: BTreeMap<FileId, BTreeSet<SymbolId>> = BTreeMap::new();
    for (symbol, file) in &entity_file {
        file_members.entry(*file).or_default().insert(*symbol);
    }

    // Order components deterministically by symbol count then file identity and
    // apply the requested component cap.
    let mut component_files: Vec<FileId> = file_members.keys().copied().collect();
    component_files.sort_by(|left, right| {
        let left_count = file_members.get(left).map_or(0, BTreeSet::len);
        let right_count = file_members.get(right).map_or(0, BTreeSet::len);
        right_count.cmp(&left_count).then_with(|| left.cmp(right))
    });
    if component_files.len() > plan.max_components {
        record_limit(limiting_resources, QueryResource::Results)?;
    }
    component_files.truncate(plan.max_components);
    let reported: BTreeSet<FileId> = component_files.iter().copied().collect();

    let mut components: Vec<ArchitectureComponent> = Vec::new();
    for file in &component_files {
        let Some(members) = file_members.get(file) else {
            continue;
        };
        let mut kinds: BTreeSet<String> = BTreeSet::new();
        for symbol in members {
            if let Some(kind) = entity_kind.get(symbol) {
                kinds.insert(kind.clone());
            }
        }
        let mut responsibility_evidence: Vec<String> = Vec::new();
        responsibility_evidence.push("contains_symbols".to_owned());
        for kind in &kinds {
            responsibility_evidence.push(format!("entity_kind:{kind}"));
        }
        responsibility_evidence.truncate(16);
        let component = ArchitectureComponent {
            id: file.to_string(),
            kind: "file".to_owned(),
            name: architecture_file_name(document, *file),
            symbol_count: u32::try_from(members.len()).unwrap_or(u32::MAX),
            responsibility_evidence,
            confidence: file_confidence.get(file).copied().unwrap_or(0),
        };
        emit_cycle_value(
            &mut components,
            component,
            tracker,
            limiting_resources,
            control,
        )?;
    }

    // Aggregate served entity edges into connections between distinct reported
    // components, keyed by source file, target file, and relation family.
    let mut aggregated: BTreeMap<(FileId, FileId, RelationFamily), (u32, u16)> = BTreeMap::new();
    for (subject, object, family, confidence) in &raw_edges {
        let (Some(from), Some(to)) = (entity_file.get(subject), entity_file.get(object)) else {
            continue;
        };
        if from == to || !reported.contains(from) || !reported.contains(to) {
            continue;
        }
        let entry = aggregated.entry((*from, *to, *family)).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(1);
        if *confidence > entry.1 {
            entry.1 = *confidence;
        }
    }

    let mut connections: Vec<ArchitectureConnection> = Vec::new();
    for ((from, to, family), (weight, confidence)) in &aggregated {
        let connection = ArchitectureConnection {
            from: from.to_string(),
            to: to.to_string(),
            kind: *family,
            weight: *weight,
            confidence: *confidence,
        };
        emit_cycle_value(
            &mut connections,
            connection,
            tracker,
            limiting_resources,
            control,
        )?;
    }

    // Rank reported components by structural fan-in and fan-out, normalizing
    // the score so the busiest component scores 1000.
    let mut fan_in: BTreeMap<FileId, u32> = BTreeMap::new();
    let mut fan_out: BTreeMap<FileId, u32> = BTreeMap::new();
    for (from, to, _family) in aggregated.keys() {
        let outbound = fan_out.entry(*from).or_insert(0);
        *outbound = outbound.saturating_add(1);
        let inbound = fan_in.entry(*to).or_insert(0);
        *inbound = inbound.saturating_add(1);
    }
    let max_total = component_files
        .iter()
        .map(|file| {
            fan_in
                .get(file)
                .copied()
                .unwrap_or(0)
                .saturating_add(fan_out.get(file).copied().unwrap_or(0))
        })
        .max()
        .unwrap_or(0);
    let mut ranked: Vec<(FileId, u32, u32, u16)> = Vec::new();
    for file in &component_files {
        let inbound = fan_in.get(file).copied().unwrap_or(0);
        let outbound = fan_out.get(file).copied().unwrap_or(0);
        let total = inbound.saturating_add(outbound);
        if total == 0 {
            continue;
        }
        let score = if max_total == 0 {
            0
        } else {
            u16::try_from(u64::from(total) * 1_000 / u64::from(max_total)).unwrap_or(1_000)
        };
        ranked.push((*file, inbound, outbound, score));
    }
    ranked.sort_by(|left, right| {
        right
            .3
            .cmp(&left.3)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.0.cmp(&right.0))
    });
    let mut hotspots: Vec<ArchitectureHotspot> = Vec::new();
    for (file, inbound, outbound, score) in ranked {
        let hotspot = ArchitectureHotspot {
            component_id: file.to_string(),
            fan_in: inbound,
            fan_out: outbound,
            change_frequency: None,
            complexity: None,
            score,
        };
        emit_cycle_value(&mut hotspots, hotspot, tracker, limiting_resources, control)?;
    }

    let mut views: Vec<ArchitectureOverviewDerivedView> = Vec::new();
    for view in &plan.views {
        let derived = ArchitectureOverviewDerivedView {
            view: *view,
            algorithm_version: architecture_overview_algorithm_version(*view).to_owned(),
        };
        emit_cycle_value(&mut views, derived, tracker, limiting_resources, control)?;
    }

    Ok(ArchitectureOverviewAnalysis {
        components,
        connections,
        hotspots,
        views,
    })
}

/// Served relation families used to relate tests to seed symbols.
///
/// Each family maps to a disjoint IR predicate set, so a served relation
/// contributes to exactly one direct-edge rationale. `CalledBy` is intentionally
/// omitted because it shares the `Calls` predicate and would double-count the
/// same directed edge.
const TESTS_SELECT_FAMILIES: &[RelationFamily] = &[
    RelationFamily::Calls,
    RelationFamily::References,
    RelationFamily::Types,
    RelationFamily::Implements,
    RelationFamily::Imports,
];

/// Maximum honest coverage gaps reported by one `tests.select`.
const TESTS_SELECT_MAX_GAPS: usize = 128;

/// Test selection assembled before bounded result emission.
struct TestsSelectAnalysis {
    tests: Vec<RankedTestSelection>,
    coverage_strategy: TestsSelectCoverage,
    gaps: Vec<TestsSelectGap>,
}

/// One scored test candidate ordered before the bounded result cap.
struct TestsSelectScored {
    test_id: SymbolId,
    kind: TestsSelectKind,
    score: u16,
    why: Vec<String>,
}

/// Returns the honest test granularity for one normalized test entity.
///
/// The first-slice lexical oracle records a test as an entity kind or flag but
/// cannot distinguish integration, end-to-end, or contract tests, so every
/// detected test entity is reported as unit-level.
fn test_kind_for_entity(_entity: &rootlight_ir::EntityRecord) -> TestsSelectKind {
    TestsSelectKind::Unit
}

/// Computes a deterministic relevance score from the served signals.
///
/// Direct edges rank above transitive paths, which rank above file co-location,
/// and each served signal is confidence-weighted within its disjoint band so the
/// ordering direct > transitive > co-location always holds.
fn tests_select_score(direct_confidence: u16, transitive_confidence: u16, colocated: bool) -> u16 {
    if direct_confidence > 0 {
        // Direct band: 700 through 1000.
        return 700 + u16::try_from(u32::from(direct_confidence) * 300 / 1_000).unwrap_or(300);
    }
    if transitive_confidence > 0 {
        // Transitive band: 400 through 600.
        return 400 + u16::try_from(u32::from(transitive_confidence) * 200 / 1_000).unwrap_or(200);
    }
    if colocated {
        // Co-location band: a fixed honest floor.
        return 150;
    }
    0
}

/// Builds a bounded test selection for the requested seed set.
///
/// Test entities are identified from normalized entity kinds and flags and
/// related to the seeds through three honest signals: a direct served edge into
/// a seed, a bounded two-hop transitive path to a seed, and file co-location
/// with a seed. Candidates are ranked by a confidence-weighted score, capped
/// deterministically, and seeds with no related test are reported as gaps.
fn build_tests_select(
    document: &NormalizedIrDocument,
    plan: &TestsSelectPlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<TestsSelectAnalysis, QueryError> {
    // Identify test entities and resolve each entity's declaring file from
    // immutable source evidence.
    let mut entity_file: BTreeMap<SymbolId, FileId> = BTreeMap::new();
    let mut tests: BTreeMap<SymbolId, TestsSelectKind> = BTreeMap::new();
    for entity in &document.entities {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if let Some(source) = entity.evidence.source.as_ref() {
            entity_file.insert(entity.id, source.span().file());
        }
        if entity_is_test(entity) {
            tests.insert(entity.id, test_kind_for_entity(entity));
        }
    }

    // Single bounded relation scan: `Contains` relations confirm the owning file
    // of each entity, while served family relations contribute the outbound
    // adjacency used for the direct and transitive signals.
    let allowed: BTreeSet<RelationPredicate> = TESTS_SELECT_FAMILIES
        .iter()
        .flat_map(|family| family.predicates().iter().copied())
        .collect();
    let mut out_adj: BTreeMap<SymbolId, Vec<(SymbolId, RelationFamily, u16)>> = BTreeMap::new();
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if relation.predicate == RelationPredicate::Contains {
            if let (RelationEndpoint::File(file), RelationEndpoint::Entity(symbol)) =
                (relation.subject, relation.object)
            {
                entity_file.insert(symbol, file);
            }
            continue;
        }
        if !allowed.contains(&relation.predicate) {
            continue;
        }
        let Some(family) = predicate_family(TESTS_SELECT_FAMILIES, relation.predicate) else {
            continue;
        };
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        if subject == object {
            continue;
        }
        let confidence = relation.confidence.get();
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            break;
        }
        tracker.add_edges(1)?;
        out_adj
            .entry(subject)
            .or_default()
            .push((object, family, confidence));
    }

    // Resolve the file set occupied by the seeds for the co-location signal.
    let mut seed_files: BTreeSet<FileId> = BTreeSet::new();
    for seed in &plan.seeds {
        if let Some(file) = entity_file.get(seed) {
            seed_files.insert(*file);
        }
    }

    let requested_kinds: BTreeSet<TestsSelectKind> = plan.test_kinds.iter().copied().collect();

    // Score every test entity that matches the requested kind filter.
    let mut scored: Vec<TestsSelectScored> = Vec::new();
    let mut any_direct = false;
    let mut any_transitive = false;
    let mut any_colocated = false;
    let mut covered_seeds: BTreeSet<SymbolId> = BTreeSet::new();
    for (test_id, kind) in &tests {
        control.check()?;
        if !requested_kinds.is_empty() && !requested_kinds.contains(kind) {
            continue;
        }
        let edges = out_adj.get(test_id).map(Vec::as_slice).unwrap_or(&[]);
        // Direct signal: strongest outbound edge into a seed.
        let mut direct_confidence = 0_u16;
        let mut direct_family: Option<RelationFamily> = None;
        for (target, family, confidence) in edges {
            if plan.seeds.contains(target) && *confidence > direct_confidence {
                direct_confidence = *confidence;
                direct_family = Some(*family);
                covered_seeds.insert(*target);
            }
        }
        // Transitive signal: strongest two-hop path test -> node -> seed,
        // weighted by the weakest edge on the path.
        let mut transitive_confidence = 0_u16;
        if direct_confidence == 0 {
            for (mid, _family, first_confidence) in edges {
                if plan.seeds.contains(mid) {
                    continue;
                }
                let Some(second_hop) = out_adj.get(mid) else {
                    continue;
                };
                for (target, _second_family, second_confidence) in second_hop {
                    if !plan.seeds.contains(target) {
                        continue;
                    }
                    let path_confidence = (*first_confidence).min(*second_confidence);
                    if path_confidence > transitive_confidence {
                        transitive_confidence = path_confidence;
                        covered_seeds.insert(*target);
                    }
                }
            }
        }
        // Co-location signal: the test shares a declaring file with a seed.
        let colocated = entity_file
            .get(test_id)
            .is_some_and(|file| seed_files.contains(file));
        if colocated && let Some(test_file) = entity_file.get(test_id) {
            for seed in &plan.seeds {
                if entity_file.get(seed) == Some(test_file) {
                    covered_seeds.insert(*seed);
                }
            }
        }

        let direct = direct_confidence > 0;
        let transitive = transitive_confidence > 0 && !direct;
        if direct {
            any_direct = true;
        }
        if transitive {
            any_transitive = true;
        }
        if colocated {
            any_colocated = true;
        }
        if !direct && !transitive && !colocated {
            continue;
        }

        let score = tests_select_score(direct_confidence, transitive_confidence, colocated);
        let mut why = Vec::new();
        if direct {
            why.push("direct_test_edge".to_owned());
            if let Some(family) = direct_family {
                why.push(format!("via:{}", family.as_str()));
            }
        }
        if transitive {
            why.push("transitive_dependency".to_owned());
        }
        if colocated {
            why.push("shared_file_with_seed".to_owned());
        }
        why.truncate(8);
        scored.push(TestsSelectScored {
            test_id: *test_id,
            kind: *kind,
            score,
            why,
        });
    }

    // Rank deterministically by score then identity and apply the test cap.
    scored.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.test_id.cmp(&right.test_id))
    });
    let mut ranked_tests: Vec<RankedTestSelection> = Vec::new();
    for entry in scored {
        if ranked_tests.len() >= plan.max_tests {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        let path = entity_file
            .get(&entry.test_id)
            .and_then(|file| find_file(document, *file))
            .map(|record| record.path.clone());
        let command_hint = plan
            .include_commands
            .then(|| format!("test:{}", entry.kind.as_str()));
        let ranked = RankedTestSelection {
            test_id: entry.test_id,
            kind: entry.kind,
            path,
            score: entry.score,
            why: entry.why,
            estimated_cost_ms: None,
            command_hint,
        };
        emit_cycle_value(
            &mut ranked_tests,
            ranked,
            tracker,
            limiting_resources,
            control,
        )?;
    }

    // Report an honest gap for every seed scope with no related test.
    let mut gaps: Vec<TestsSelectGap> = Vec::new();
    for seed in &plan.seeds {
        if covered_seeds.contains(seed) {
            continue;
        }
        if gaps.len() >= TESTS_SELECT_MAX_GAPS {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        let gap = TestsSelectGap {
            scope: seed.to_string(),
            reason: "no_related_test".to_owned(),
        };
        emit_cycle_value(&mut gaps, gap, tracker, limiting_resources, control)?;
    }

    Ok(TestsSelectAnalysis {
        tests: ranked_tests,
        coverage_strategy: TestsSelectCoverage {
            direct_edges: any_direct,
            transitive_signals: any_transitive,
            history_signals: false,
            file_colocation_signals: any_colocated,
        },
        gaps,
    })
}

/// Served relation families used to propagate change impact to dependents.
///
/// Each family maps to a disjoint IR predicate set, so a served relation
/// contributes to exactly one impact-path predicate. `CalledBy` is intentionally
/// omitted because it shares the `Calls` predicate and would double-count the
/// same directed edge.
const CHANGE_IMPACT_FAMILIES: &[RelationFamily] = &[
    RelationFamily::Calls,
    RelationFamily::References,
    RelationFamily::Types,
    RelationFamily::Implements,
    RelationFamily::Imports,
];

/// Maximum resolved changes reported by one `change.impact`.
const CHANGE_IMPACT_MAX_RESOLVED: usize = 1_256;

/// Maximum test candidates reported by one `change.impact`.
const CHANGE_IMPACT_MAX_TESTS: usize = 500;

/// Change impact assembled before bounded result emission.
struct ChangeImpactAnalysis {
    resolved_changes: Vec<ResolvedChangeRecord>,
    impacted: Vec<ImpactGroupRecord>,
    tests: Vec<ChangeImpactTestCandidate>,
    risk_summary: ChangeImpactRiskSummary,
}

/// Builds a bounded change-impact analysis for the explicit change set.
///
/// The explicit symbols and paths are resolved to concrete changes, a reverse
/// dependent graph is built over the served relation families, a bounded
/// forward closure propagates each change to its dependents, test entities are
/// optionally related to the impacted symbols, and an honest risk summary is
/// aggregated. Rows, edges, results, and memory are bounded exactly like
/// `tests.select`.
fn build_change_impact(
    document: &NormalizedIrDocument,
    plan: &ChangeImpactPlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<ChangeImpactAnalysis, QueryError> {
    // Resolve per-entity metadata: declaring file, kind label, and public
    // surface membership, plus the path-to-file map used to resolve explicit
    // path changes.
    let mut entity_file: BTreeMap<SymbolId, FileId> = BTreeMap::new();
    let mut entity_kind: BTreeMap<SymbolId, String> = BTreeMap::new();
    let mut entity_public: BTreeSet<SymbolId> = BTreeSet::new();
    for entity in &document.entities {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if let Some(source) = entity.evidence.source.as_ref() {
            entity_file.insert(entity.id, source.span().file());
        }
        entity_kind.insert(entity.id, serialized_label(&entity.kind)?);
        if entity_is_exported(entity) {
            entity_public.insert(entity.id);
        }
    }

    let mut path_to_file: BTreeMap<String, FileId> = BTreeMap::new();
    for file in &document.files {
        path_to_file.insert(file.path.clone(), file.id);
    }

    // Single bounded relation scan: `Contains` relations confirm the owning file
    // of each entity, while served family relations contribute the reverse
    // dependent adjacency (a subject edge into an object makes the subject a
    // dependent of the object).
    let allowed: BTreeSet<RelationPredicate> = CHANGE_IMPACT_FAMILIES
        .iter()
        .flat_map(|family| family.predicates().iter().copied())
        .collect();
    let mut dependents: BTreeMap<SymbolId, Vec<(SymbolId, RelationFamily, u16)>> = BTreeMap::new();
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if relation.predicate == RelationPredicate::Contains {
            if let (RelationEndpoint::File(file), RelationEndpoint::Entity(symbol)) =
                (relation.subject, relation.object)
            {
                entity_file.insert(symbol, file);
            }
            continue;
        }
        if !allowed.contains(&relation.predicate) {
            continue;
        }
        let confidence = relation.confidence.get();
        if confidence < plan.min_confidence {
            continue;
        }
        let Some(family) = predicate_family(CHANGE_IMPACT_FAMILIES, relation.predicate) else {
            continue;
        };
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        if subject == object {
            continue;
        }
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            break;
        }
        tracker.add_edges(1)?;
        dependents
            .entry(object)
            .or_default()
            .push((subject, family, confidence));
    }
    for edges in dependents.values_mut() {
        edges.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.as_str().cmp(right.1.as_str()))
                .then_with(|| right.2.cmp(&left.2))
        });
    }

    // Build the file-to-entity map after containment is fully resolved.
    let mut file_entities: BTreeMap<FileId, BTreeSet<SymbolId>> = BTreeMap::new();
    for (symbol, file) in &entity_file {
        file_entities.entry(*file).or_default().insert(*symbol);
    }

    // Resolve the explicit change set to concrete resolved changes.
    let resolved_changes = resolve_changed_set(
        plan,
        &entity_file,
        &entity_kind,
        &entity_public,
        &file_entities,
        &path_to_file,
        tracker,
        limiting_resources,
        control,
    )?;

    // Run a bounded forward impact closure from each resolved change.
    let mut impacted: Vec<ImpactGroupRecord> = Vec::new();
    for (index, change) in resolved_changes.iter().enumerate() {
        control.check()?;
        let source_index = u16::try_from(index).unwrap_or(u16::MAX);
        let Some(symbol) = change.symbol_id else {
            // A file-only or unresolved change has no symbol to propagate from;
            // report an honest empty group.
            let group = ImpactGroupRecord {
                source_index,
                dependents: Vec::new(),
            };
            emit_cycle_value(&mut impacted, group, tracker, limiting_resources, control)?;
            continue;
        };
        let roots = BTreeSet::from([symbol]);
        let dependents_for_change = impact_closure(
            &dependents,
            &roots,
            plan.max_depth,
            &entity_kind,
            &entity_public,
            plan.max_dependents,
            tracker,
            limiting_resources,
            control,
        )?;
        let group = ImpactGroupRecord {
            source_index,
            dependents: dependents_for_change,
        };
        emit_cycle_value(&mut impacted, group, tracker, limiting_resources, control)?;
    }

    // Relate test entities to the impacted symbols when requested.
    let tests = if plan.include_tests {
        build_change_impact_tests(
            document,
            plan,
            &resolved_changes,
            &impacted,
            control,
            tracker,
            limiting_resources,
        )?
    } else {
        Vec::new()
    };

    let risk_summary = change_impact_risk_summary(&resolved_changes, &impacted);

    Ok(ChangeImpactAnalysis {
        resolved_changes,
        impacted,
        tests,
        risk_summary,
    })
}

/// Resolves the explicit change set to concrete resolved changes.
///
/// Each explicit symbol maps to one resolved change classified by its public
/// surface membership; an unknown symbol still resolves to a body-classified
/// change so the caller's asserted change is not silently dropped. Each explicit
/// path maps to the entities declared in the matching file, to a file-only
/// change when the file is known but declares no served entity, or to a
/// fully-unresolved change when the path is unknown.
#[expect(
    clippy::too_many_arguments,
    reason = "the resolver carries the resolved entity maps plus bounded budget and control state"
)]
fn resolve_changed_set(
    plan: &ChangeImpactPlan,
    entity_file: &BTreeMap<SymbolId, FileId>,
    entity_kind: &BTreeMap<SymbolId, String>,
    entity_public: &BTreeSet<SymbolId>,
    file_entities: &BTreeMap<FileId, BTreeSet<SymbolId>>,
    path_to_file: &BTreeMap<String, FileId>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<Vec<ResolvedChangeRecord>, QueryError> {
    let mut resolved: Vec<ResolvedChangeRecord> = Vec::new();
    // Explicit symbols first, in deterministic identity order.
    for symbol in &plan.changed_symbols {
        control.check()?;
        if resolved.len() >= CHANGE_IMPACT_MAX_RESOLVED {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        let classification = if entity_public.contains(symbol) {
            ChangeImpactClassification::Surface
        } else {
            ChangeImpactClassification::Body
        };
        let record = ResolvedChangeRecord {
            symbol_id: Some(*symbol),
            file_id: entity_file.get(symbol).copied(),
            classification,
            kind: entity_kind.get(symbol).cloned(),
        };
        emit_cycle_value(&mut resolved, record, tracker, limiting_resources, control)?;
    }
    // Explicit paths, in deterministic sorted order.
    for path in &plan.changed_paths {
        control.check()?;
        if resolved.len() >= CHANGE_IMPACT_MAX_RESOLVED {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        let Some(file) = path_to_file.get(path).copied() else {
            // The path is not part of the indexed generation; report an honest
            // fully-unresolved change rather than dropping the caller's input.
            let record = ResolvedChangeRecord {
                symbol_id: None,
                file_id: None,
                classification: ChangeImpactClassification::Body,
                kind: None,
            };
            emit_cycle_value(&mut resolved, record, tracker, limiting_resources, control)?;
            continue;
        };
        let declared = file_entities.get(&file).cloned().unwrap_or_default();
        if declared.is_empty() {
            let record = ResolvedChangeRecord {
                symbol_id: None,
                file_id: Some(file),
                classification: ChangeImpactClassification::Body,
                kind: None,
            };
            emit_cycle_value(&mut resolved, record, tracker, limiting_resources, control)?;
            continue;
        }
        for symbol in declared {
            control.check()?;
            if resolved.len() >= CHANGE_IMPACT_MAX_RESOLVED {
                record_limit(limiting_resources, QueryResource::Results)?;
                break;
            }
            let classification = if entity_public.contains(&symbol) {
                ChangeImpactClassification::Surface
            } else {
                ChangeImpactClassification::Body
            };
            let record = ResolvedChangeRecord {
                symbol_id: Some(symbol),
                file_id: Some(file),
                classification,
                kind: entity_kind.get(&symbol).cloned(),
            };
            emit_cycle_value(&mut resolved, record, tracker, limiting_resources, control)?;
        }
    }
    Ok(resolved)
}

/// Runs a bounded forward impact closure from the changed roots.
///
/// The reverse dependent adjacency maps each symbol to the symbols that depend
/// on it; a breadth-first traversal from the roots records each reached
/// dependent's shortest distance, weakest-edge confidence, and predicate path.
/// Dependents are emitted ordered by distance then identity under the dependent
/// cap.
#[expect(
    clippy::too_many_arguments,
    reason = "the closure carries the dependent graph plus resolved entity maps and bounded budget state"
)]
fn impact_closure(
    dependents: &BTreeMap<SymbolId, Vec<(SymbolId, RelationFamily, u16)>>,
    roots: &BTreeSet<SymbolId>,
    max_depth: u8,
    entity_kind: &BTreeMap<SymbolId, String>,
    entity_public: &BTreeSet<SymbolId>,
    max_dependents: usize,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<Vec<ImpactEntryRecord>, QueryError> {
    // First-visit wins under breadth-first order, so each dependent records its
    // shortest distance and the predicate path that reached it.
    let mut visited: BTreeMap<SymbolId, (u8, u16, Vec<String>)> = BTreeMap::new();
    let mut queue: VecDeque<(SymbolId, u8, u16, Vec<String>)> = VecDeque::new();
    for root in roots {
        queue.push_back((*root, 0, 1_000, Vec::new()));
    }
    while let Some((node, distance, confidence, via)) = queue.pop_front() {
        control.check()?;
        if distance >= max_depth {
            if dependents.get(&node).is_some_and(|edges| {
                edges.iter().any(|(subject, _, _)| {
                    !roots.contains(subject) && !visited.contains_key(subject)
                })
            }) {
                record_limit(limiting_resources, QueryResource::Depth)?;
            }
            continue;
        }
        let next_distance = distance.saturating_add(1);
        let Some(edges) = dependents.get(&node) else {
            continue;
        };
        for (subject, family, edge_confidence) in edges {
            if roots.contains(subject) || visited.contains_key(subject) {
                continue;
            }
            let path_confidence = confidence.min(*edge_confidence);
            let mut path = via.clone();
            path.push(family.as_str().to_owned());
            path.truncate(16);
            visited.insert(*subject, (next_distance, path_confidence, path.clone()));
            queue.push_back((*subject, next_distance, path_confidence, path));
        }
    }

    let mut reached: Vec<(SymbolId, u8, u16, Vec<String>)> = visited
        .into_iter()
        .map(|(symbol, (distance, confidence, via))| (symbol, distance, confidence, via))
        .collect();
    reached.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));

    let mut entries: Vec<ImpactEntryRecord> = Vec::new();
    for (symbol, distance, confidence, via) in reached {
        if entries.len() >= max_dependents {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        let kind = entity_kind
            .get(&symbol)
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned());
        let entry = ImpactEntryRecord {
            symbol_id: symbol,
            kind,
            distance,
            confidence,
            via,
            is_public: entity_public.contains(&symbol),
        };
        emit_cycle_value(&mut entries, entry, tracker, limiting_resources, control)?;
    }
    Ok(entries)
}

/// Relates test entities to the changed and impacted symbols.
///
/// The reused `tests.select` ranking is seeded from the resolved change symbols
/// and every impacted dependent, so a test related to either is surfaced with
/// the same honest direct, transitive, and co-location signals.
fn build_change_impact_tests(
    document: &NormalizedIrDocument,
    plan: &ChangeImpactPlan,
    resolved_changes: &[ResolvedChangeRecord],
    impacted: &[ImpactGroupRecord],
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<Vec<ChangeImpactTestCandidate>, QueryError> {
    let mut seeds: BTreeSet<SymbolId> = BTreeSet::new();
    for change in resolved_changes {
        if let Some(symbol) = change.symbol_id {
            seeds.insert(symbol);
        }
    }
    for group in impacted {
        for entry in &group.dependents {
            seeds.insert(entry.symbol_id);
        }
    }
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    // The reused selection admits a bounded seed set; keep the smallest
    // identities deterministically when the impacted surface is larger.
    if seeds.len() > 64 {
        seeds = seeds.into_iter().take(64).collect();
    }
    let selection_plan = TestsSelectPlan {
        seeds,
        test_kinds: Vec::new(),
        max_tests: CHANGE_IMPACT_MAX_TESTS,
        include_commands: false,
        budget: plan.budget,
        explanation: plan.explanation.clone(),
    };
    let selection = build_tests_select(
        document,
        &selection_plan,
        control,
        tracker,
        limiting_resources,
    )?;
    let mut tests: Vec<ChangeImpactTestCandidate> = Vec::new();
    for ranked in selection.tests {
        if tests.len() >= CHANGE_IMPACT_MAX_TESTS {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        let candidate = ChangeImpactTestCandidate {
            test_id: ranked.test_id.to_string(),
            relevance: ranked.score,
            why: ranked.why,
            estimated_cost_ms: ranked.estimated_cost_ms,
        };
        emit_cycle_value(&mut tests, candidate, tracker, limiting_resources, control)?;
    }
    Ok(tests)
}

/// Aggregates an honest risk summary from the resolved changes and impact groups.
///
/// The fanout counts every reported dependent, the breaking surface records
/// whether any public symbol was changed or impacted, and the level orders local
/// changes below cross-module fanout below public-surface effects. Coverage is
/// always unknown because the lexical oracle cannot establish completeness, and
/// dynamic blind spots are always reported.
fn change_impact_risk_summary(
    resolved_changes: &[ResolvedChangeRecord],
    impacted: &[ImpactGroupRecord],
) -> ChangeImpactRiskSummary {
    let fanout = u32::try_from(
        impacted
            .iter()
            .map(|group| group.dependents.len())
            .sum::<usize>(),
    )
    .unwrap_or(u32::MAX)
    .min(100_000);
    let breaking_surface = resolved_changes
        .iter()
        .any(|change| matches!(change.classification, ChangeImpactClassification::Surface))
        || impacted
            .iter()
            .any(|group| group.dependents.iter().any(|entry| entry.is_public));
    let level = if breaking_surface && fanout >= 20 {
        ChangeImpactRiskLevel::Critical
    } else if breaking_surface {
        ChangeImpactRiskLevel::High
    } else if fanout >= 20 {
        ChangeImpactRiskLevel::Medium
    } else if fanout > 0 {
        ChangeImpactRiskLevel::Low
    } else {
        ChangeImpactRiskLevel::None
    };
    let mut reasons: Vec<String> = Vec::new();
    if breaking_surface {
        reasons.push("public_surface_affected".to_owned());
    }
    if fanout > 0 {
        reasons.push("transitive_fanout".to_owned());
    } else {
        reasons.push("no_measured_impact".to_owned());
    }
    // The lexical oracle never resolves dynamic dispatch or reflection, so every
    // result carries an honest blind-spot caveat.
    reasons.push("dynamic_dispatch_blind_spot".to_owned());
    reasons.truncate(16);
    ChangeImpactRiskSummary {
        level,
        reasons,
        coverage: CoverageStatus::Unknown,
        breaking_surface,
        fanout,
        dynamic_blind_spots: true,
    }
}

/// Default transitive depth for the reused `plan.change` impact closure.
const PLAN_CHANGE_DEFAULT_DEPTH: u8 = 3;

/// Default dependent cap for the reused `plan.change` impact closure.
const PLAN_CHANGE_DEFAULT_DEPENDENTS: usize = 100;

/// Maximum related tests carried in one `plan.change` verification plan.
const PLAN_CHANGE_MAX_TESTS: usize = 500;

/// Maximum symbols or files carried in one `plan.change` context pack.
const PLAN_CHANGE_MAX_CONTEXT_ITEMS: usize = 64;

/// Maximum target symbols attached to one `plan.change` step.
const PLAN_CHANGE_MAX_STEP_TARGETS: usize = 32;

/// Change plan assembled before bounded result emission.
struct PlanChangeAnalysis {
    plan: Vec<PlanChangeStepRecord>,
    affected_scope: PlanChangeImpactSummary,
    test_plan: Vec<ChangeImpactTestCandidate>,
    open_decisions: Vec<PlanChangeDecision>,
    context_pack_request: PlanChangeContextPack,
}

/// Builds a bounded change plan for the explicit target set.
///
/// The explicit symbol and file targets are resolved to concrete symbols, the
/// reused `change.impact` forward closure propagates the targets to their
/// dependents over the served relation families, the reused `tests.select`
/// ranking relates test entities to the impacted surface, and a deterministic
/// ordered plan is built from the objective class and the measured impact. The
/// impact summary, open decisions, and context-pack request are all source-free
/// and honest: no dependent or repository content is fabricated. Rows, edges,
/// results, and memory are bounded exactly like `change.impact`.
fn build_plan_change(
    document: &NormalizedIrDocument,
    plan: &PlanChangePlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<PlanChangeAnalysis, QueryError> {
    // Resolve per-entity metadata: declaring file, kind label, and public
    // surface membership, exactly like `change.impact`.
    let mut entity_file: BTreeMap<SymbolId, FileId> = BTreeMap::new();
    let mut entity_kind: BTreeMap<SymbolId, String> = BTreeMap::new();
    let mut entity_public: BTreeSet<SymbolId> = BTreeSet::new();
    for entity in &document.entities {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if let Some(source) = entity.evidence.source.as_ref() {
            entity_file.insert(entity.id, source.span().file());
        }
        entity_kind.insert(entity.id, serialized_label(&entity.kind)?);
        if entity_is_exported(entity) {
            entity_public.insert(entity.id);
        }
    }

    // Single bounded relation scan: `Contains` relations confirm the owning file
    // of each entity, while served family relations contribute the reverse
    // dependent adjacency reused from `change.impact`.
    let allowed: BTreeSet<RelationPredicate> = CHANGE_IMPACT_FAMILIES
        .iter()
        .flat_map(|family| family.predicates().iter().copied())
        .collect();
    let mut dependents: BTreeMap<SymbolId, Vec<(SymbolId, RelationFamily, u16)>> = BTreeMap::new();
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if relation.predicate == RelationPredicate::Contains {
            if let (RelationEndpoint::File(file), RelationEndpoint::Entity(symbol)) =
                (relation.subject, relation.object)
            {
                entity_file.insert(symbol, file);
            }
            continue;
        }
        if !allowed.contains(&relation.predicate) {
            continue;
        }
        let Some(family) = predicate_family(CHANGE_IMPACT_FAMILIES, relation.predicate) else {
            continue;
        };
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        if subject == object {
            continue;
        }
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            break;
        }
        tracker.add_edges(1)?;
        dependents
            .entry(object)
            .or_default()
            .push((subject, family, relation.confidence.get()));
    }
    for edges in dependents.values_mut() {
        edges.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.as_str().cmp(right.1.as_str()))
                .then_with(|| right.2.cmp(&left.2))
        });
    }

    // Build the file-to-entity map after containment is fully resolved.
    let mut file_entities: BTreeMap<FileId, BTreeSet<SymbolId>> = BTreeMap::new();
    for (symbol, file) in &entity_file {
        file_entities.entry(*file).or_default().insert(*symbol);
    }

    // Resolve the explicit targets to concrete symbols: symbol targets carry
    // their identity directly, while file targets expand to the entities
    // declared in the matching file.
    let mut resolved_targets: BTreeSet<SymbolId> = BTreeSet::new();
    for symbol in &plan.target_symbols {
        resolved_targets.insert(*symbol);
    }
    let mut resolved_target_files: BTreeSet<FileId> = BTreeSet::new();
    for file in &plan.target_files {
        resolved_target_files.insert(*file);
        if let Some(declared) = file_entities.get(file) {
            for symbol in declared {
                resolved_targets.insert(*symbol);
            }
        }
    }

    // Run the reused bounded forward impact closure from the resolved targets.
    let closure = impact_closure(
        &dependents,
        &resolved_targets,
        plan.max_depth,
        &entity_kind,
        &entity_public,
        plan.max_dependents,
        tracker,
        limiting_resources,
        control,
    )?;

    // Relate test entities to the targets and impacted dependents through the
    // reused tests.select ranking.
    let selection = build_plan_change_tests(
        document,
        plan,
        &resolved_targets,
        &closure,
        control,
        tracker,
        limiting_resources,
    )?;
    let test_symbols: Vec<SymbolId> = selection
        .tests
        .iter()
        .map(|ranked| ranked.test_id)
        .take(PLAN_CHANGE_MAX_STEP_TARGETS)
        .collect();
    let mut test_plan: Vec<ChangeImpactTestCandidate> = Vec::new();
    for ranked in selection.tests {
        if test_plan.len() >= PLAN_CHANGE_MAX_TESTS {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        let candidate = ChangeImpactTestCandidate {
            test_id: ranked.test_id.to_string(),
            relevance: ranked.score,
            why: ranked.why,
            estimated_cost_ms: ranked.estimated_cost_ms,
        };
        emit_cycle_value(
            &mut test_plan,
            candidate,
            tracker,
            limiting_resources,
            control,
        )?;
    }

    let affected_scope =
        plan_change_impact_summary(&resolved_targets, &closure, &entity_file, &entity_public);

    let (plan_steps, steps_truncated) = build_plan_change_steps(
        plan.objective,
        &plan.objective_text,
        &resolved_targets,
        &closure,
        &test_symbols,
        &affected_scope,
        plan.max_steps,
    );
    if steps_truncated {
        record_limit(limiting_resources, QueryResource::Results)?;
    }

    let open_decisions = plan_change_decisions(plan.objective, &affected_scope);

    let context_pack_request = plan_change_context_pack(
        &resolved_targets,
        &closure,
        &resolved_target_files,
        &entity_file,
    );

    Ok(PlanChangeAnalysis {
        plan: plan_steps,
        affected_scope,
        test_plan,
        open_decisions,
        context_pack_request,
    })
}

/// Relates test entities to the targets and impacted dependents.
///
/// The reused `tests.select` ranking is seeded from the resolved targets and
/// every reached dependent, so a test related to either is surfaced with the
/// same honest direct, transitive, and co-location signals.
fn build_plan_change_tests(
    document: &NormalizedIrDocument,
    plan: &PlanChangePlan,
    resolved_targets: &BTreeSet<SymbolId>,
    closure: &[ImpactEntryRecord],
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<TestsSelectAnalysis, QueryError> {
    let mut seeds: BTreeSet<SymbolId> = resolved_targets.clone();
    for entry in closure {
        seeds.insert(entry.symbol_id);
    }
    if seeds.is_empty() {
        return Ok(TestsSelectAnalysis {
            tests: Vec::new(),
            coverage_strategy: TestsSelectCoverage {
                direct_edges: false,
                transitive_signals: false,
                history_signals: false,
                file_colocation_signals: false,
            },
            gaps: Vec::new(),
        });
    }
    // The reused selection admits a bounded seed set; keep the smallest
    // identities deterministically when the impacted surface is larger.
    if seeds.len() > 64 {
        seeds = seeds.into_iter().take(64).collect();
    }
    let selection_plan = TestsSelectPlan {
        seeds,
        test_kinds: Vec::new(),
        max_tests: PLAN_CHANGE_MAX_TESTS,
        include_commands: false,
        budget: plan.budget,
        explanation: plan.explanation.clone(),
    };
    build_tests_select(
        document,
        &selection_plan,
        control,
        tracker,
        limiting_resources,
    )
}

/// Aggregates an honest impact summary from the resolved targets and closure.
///
/// Affected symbols count the targets plus every reached dependent; affected
/// files count their declaring files; the risk level orders local changes below
/// cross-module fanout below public-surface effects, mirroring `change.impact`.
fn plan_change_impact_summary(
    resolved_targets: &BTreeSet<SymbolId>,
    closure: &[ImpactEntryRecord],
    entity_file: &BTreeMap<SymbolId, FileId>,
    entity_public: &BTreeSet<SymbolId>,
) -> PlanChangeImpactSummary {
    let mut affected: BTreeSet<SymbolId> = resolved_targets.clone();
    for entry in closure {
        affected.insert(entry.symbol_id);
    }
    let affected_symbols = u32::try_from(affected.len())
        .unwrap_or(u32::MAX)
        .min(100_000);
    let mut files: BTreeSet<FileId> = BTreeSet::new();
    for symbol in &affected {
        if let Some(file) = entity_file.get(symbol) {
            files.insert(*file);
        }
    }
    let affected_files = u32::try_from(files.len()).unwrap_or(u32::MAX).min(100_000);
    let touches_public_surface = affected.iter().any(|symbol| entity_public.contains(symbol));
    let fanout = u32::try_from(closure.len())
        .unwrap_or(u32::MAX)
        .min(100_000);
    let risk_level = if touches_public_surface && fanout >= 20 {
        ChangeImpactRiskLevel::Critical
    } else if touches_public_surface {
        ChangeImpactRiskLevel::High
    } else if fanout >= 20 {
        ChangeImpactRiskLevel::Medium
    } else if fanout > 0 {
        ChangeImpactRiskLevel::Low
    } else {
        ChangeImpactRiskLevel::None
    };
    PlanChangeImpactSummary {
        affected_symbols,
        affected_files,
        risk_level,
        touches_public_surface,
    }
}

/// Builds the deterministic ordered plan steps from the objective and impact.
///
/// Modification objectives emit inspect, modify, update-dependents, and
/// run-tests steps plus a public-surface confirmation when public surface is
/// touched; explanation and review objectives emit read-only inspect, trace or
/// assess, and report steps. The first action identifies the caller-authored
/// requested outcome as something to validate; risks and verification hints
/// remain source-free. The sequence is capped at `max_steps`, and every step
/// only depends on earlier ordinals so truncation keeps dependencies valid.
fn build_plan_change_steps(
    objective: PlanChangeObjective,
    objective_text: &str,
    resolved_targets: &BTreeSet<SymbolId>,
    closure: &[ImpactEntryRecord],
    test_symbols: &[SymbolId],
    affected_scope: &PlanChangeImpactSummary,
    max_steps: usize,
) -> (Vec<PlanChangeStepRecord>, bool) {
    let target_symbols: Vec<SymbolId> = resolved_targets
        .iter()
        .copied()
        .take(PLAN_CHANGE_MAX_STEP_TARGETS)
        .collect();
    let direct_dependents: Vec<SymbolId> = closure
        .iter()
        .filter(|entry| entry.distance == 1)
        .map(|entry| entry.symbol_id)
        .take(PLAN_CHANGE_MAX_STEP_TARGETS)
        .collect();
    let test_targets: Vec<SymbolId> = test_symbols
        .iter()
        .copied()
        .take(PLAN_CHANGE_MAX_STEP_TARGETS)
        .collect();

    let mut steps: Vec<PlanChangeStepRecord> = Vec::new();
    let requested_outcome =
        |action: &str| format!("{action} Validate the caller-requested outcome: {objective_text}");
    match objective {
        PlanChangeObjective::Explanation => {
            steps.push(plan_step(
                1,
                &requested_outcome(
                    "Inspect the target symbols and the relations that define their behavior.",
                ),
                target_symbols.clone(),
                Vec::new(),
                &[],
                Some("confirm the inspected behavior matches the documented intent"),
            ));
            steps.push(plan_step(
                2,
                "Trace the dependency closure to understand how the targets are used.",
                direct_dependents.clone(),
                vec![1],
                &[],
                None,
            ));
            steps.push(plan_step(
                3,
                "Summarize the observed behavior and dependencies into an explanation.",
                target_symbols.clone(),
                vec![1, 2],
                &[],
                Some("review the explanation against the inspected behavior"),
            ));
        }
        PlanChangeObjective::Review => {
            steps.push(plan_step(
                1,
                &requested_outcome("Inspect the target symbols and their current implementation."),
                target_symbols.clone(),
                Vec::new(),
                &[],
                Some("confirm the review scope covers the target symbols"),
            ));
            steps.push(plan_step(
                2,
                "Assess the impact and risk of the target symbols across their dependents.",
                direct_dependents.clone(),
                vec![1],
                &["review_scope_incomplete"],
                None,
            ));
            steps.push(plan_step(
                3,
                "Report findings and recommended follow-ups for the reviewed targets.",
                target_symbols.clone(),
                vec![1, 2],
                &[],
                Some("record findings with source-free rationale"),
            ));
        }
        PlanChangeObjective::BugFix
        | PlanChangeObjective::Refactor
        | PlanChangeObjective::Migration => {
            let (inspect_action, modify_action, modify_risk) = match objective {
                PlanChangeObjective::BugFix => (
                    "Inspect the target symbols and reproduce the reported defect.",
                    "Apply the minimal fix to the target symbols.",
                    "regression",
                ),
                PlanChangeObjective::Refactor => (
                    "Inspect the target symbols and confirm their current behavior.",
                    "Restructure the target symbols without changing observable behavior.",
                    "behavior_drift",
                ),
                // PlanChangeObjective::Migration is the only remaining arm.
                _ => (
                    "Inspect the target symbols and the API or dependency they currently use.",
                    "Migrate the target symbols to the new API or dependency.",
                    "compatibility_break",
                ),
            };
            steps.push(plan_step(
                1,
                &requested_outcome(inspect_action),
                target_symbols.clone(),
                Vec::new(),
                &[],
                Some("confirm current behavior of the target symbols"),
            ));
            steps.push(plan_step(
                2,
                modify_action,
                target_symbols.clone(),
                vec![1],
                &[modify_risk],
                None,
            ));
            steps.push(plan_step(
                3,
                "Update any direct dependents affected by the change.",
                direct_dependents.clone(),
                vec![2],
                &["dependent_breakage"],
                None,
            ));
            steps.push(plan_step(
                4,
                "Run the related tests to verify the change.",
                test_targets.clone(),
                vec![2, 3],
                &[],
                Some("run the related tests"),
            ));
            if affected_scope.touches_public_surface {
                steps.push(plan_step(
                    5,
                    "Confirm the public-surface change preserves the intended contract.",
                    target_symbols.clone(),
                    vec![2],
                    &["public_surface_break"],
                    Some("verify the public contract is preserved"),
                ));
            }
        }
    }
    let truncated = steps.len() > max_steps;
    steps.truncate(max_steps);
    (steps, truncated)
}

/// Builds one source-free ordered plan step.
fn plan_step(
    step: u8,
    action: &str,
    targets: Vec<SymbolId>,
    depends_on: Vec<u8>,
    risks: &[&str],
    verification: Option<&str>,
) -> PlanChangeStepRecord {
    PlanChangeStepRecord {
        step,
        action: action.to_owned(),
        targets,
        depends_on,
        risks: risks.iter().map(|risk| (*risk).to_owned()).collect(),
        verification: verification.map(str::to_owned),
    }
}

/// Builds the honest open decisions that cannot be safely inferred.
///
/// A public-surface change always raises a backward-compatibility confirmation,
/// and migration or refactor objectives raise a behavior-preservation
/// confirmation; every question and recommended default is source-free.
fn plan_change_decisions(
    objective: PlanChangeObjective,
    affected_scope: &PlanChangeImpactSummary,
) -> Vec<PlanChangeDecision> {
    let mut decisions: Vec<PlanChangeDecision> = Vec::new();
    if affected_scope.touches_public_surface {
        decisions.push(PlanChangeDecision {
            question: "confirm_public_surface_change".to_owned(),
            recommended_default: "preserve_backward_compatibility".to_owned(),
        });
    }
    match objective {
        PlanChangeObjective::Migration => decisions.push(PlanChangeDecision {
            question: "confirm_migration_compatibility".to_owned(),
            recommended_default: "keep_old_and_new_paths_until_verified".to_owned(),
        }),
        PlanChangeObjective::Refactor => decisions.push(PlanChangeDecision {
            question: "confirm_behavior_preservation".to_owned(),
            recommended_default: "preserve_observable_behavior".to_owned(),
        }),
        PlanChangeObjective::BugFix
        | PlanChangeObjective::Explanation
        | PlanChangeObjective::Review => {}
    }
    decisions.truncate(16);
    decisions
}

/// Builds the ready follow-up context-pack arguments.
///
/// The pack carries the resolved targets plus the reached dependents and the
/// declaring files of those symbols together with the explicit target files, all
/// in deterministic order and capped for a bounded follow-up request.
fn plan_change_context_pack(
    resolved_targets: &BTreeSet<SymbolId>,
    closure: &[ImpactEntryRecord],
    resolved_target_files: &BTreeSet<FileId>,
    entity_file: &BTreeMap<SymbolId, FileId>,
) -> PlanChangeContextPack {
    let mut symbols: BTreeSet<SymbolId> = resolved_targets.clone();
    for entry in closure {
        symbols.insert(entry.symbol_id);
    }
    let symbols: Vec<SymbolId> = symbols
        .into_iter()
        .take(PLAN_CHANGE_MAX_CONTEXT_ITEMS)
        .collect();
    let mut files: BTreeSet<FileId> = resolved_target_files.clone();
    for symbol in &symbols {
        if let Some(file) = entity_file.get(symbol) {
            files.insert(*file);
        }
    }
    let files: Vec<FileId> = files
        .into_iter()
        .take(PLAN_CHANGE_MAX_CONTEXT_ITEMS)
        .collect();
    PlanChangeContextPack { symbols, files }
}

/// Maximum semantic changes, breaking candidates, or lineage matches carried in
/// one `history.compare` result page.
const HISTORY_COMPARE_MAX_RESULTS: usize = 1_000;

/// Maximum change-kind filter categories admitted by one `history.compare` plan.
const HISTORY_COMPARE_MAX_CHANGE_KINDS: usize = 8;

/// Maximum breaking candidates carried in one `history.compare` result.
const HISTORY_COMPARE_MAX_BREAKING: usize = 256;

/// Comparable per-entity fingerprint used to diff two generations.
///
/// The fingerprint captures only what the normalized IR honestly exposes: the
/// entity kind label, public-surface membership, and the definition source span.
/// A kind or span difference is reported as a modification; rename and move
/// detection are not claimed by this slice.
struct HistoryEntityFingerprint {
    kind_label: String,
    is_public: bool,
    source_file: Option<FileId>,
    source_start: u64,
    source_end: u64,
}

impl HistoryEntityFingerprint {
    fn from_entity(entity: &rootlight_ir::EntityRecord) -> Result<Self, QueryError> {
        let span = entity.evidence.source.as_ref().map(|source| source.span());
        Ok(Self {
            kind_label: serialized_label(&entity.kind)?,
            is_public: entity_is_exported(entity),
            source_file: span.map(|span| span.file()),
            source_start: span.map_or(0, |span| span.start_byte()),
            source_end: span.map_or(0, |span| span.end_byte()),
        })
    }

    /// Returns whether the kind or definition source span differs from `other`.
    fn signature_differs(&self, other: &Self) -> bool {
        self.kind_label != other.kind_label
            || self.source_file != other.source_file
            || self.source_start != other.source_start
            || self.source_end != other.source_end
    }
}

/// Bounded `history.compare` analysis assembled before result emission.
struct HistoryCompareAnalysis {
    coverage: CoverageStatus,
    changes: Vec<SemanticChangeRecord>,
    architecture_delta: HistoryArchitectureDelta,
    breaking_candidates: Vec<BreakingCandidateRecord>,
    lineage: Vec<LineageMatchRecord>,
}

/// Builds a bounded semantic comparison between two generation documents.
///
/// The base and head entity sets are indexed by stable identity and diffed into
/// added, removed, and modified changes. Identity-preserved symbols form honest
/// lineage matches. Removed or modified public-surface symbols become breaking
/// candidates ranked by their base-generation consumer count. The architecture
/// delta is an honest zero because this slice models no service or boundary
/// graph. Rows, edges, results, and memory are bounded exactly like
/// `change.impact`.
fn build_history_compare(
    base_document: &NormalizedIrDocument,
    head_document: &NormalizedIrDocument,
    plan: &HistoryComparePlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<HistoryCompareAnalysis, QueryError> {
    let base_entities = history_entity_index(base_document, control, tracker, limiting_resources)?;
    let head_entities = history_entity_index(head_document, control, tracker, limiting_resources)?;

    // Union of every observed identity in deterministic order.
    let mut identities: BTreeSet<SymbolId> = BTreeSet::new();
    identities.extend(base_entities.keys().copied());
    identities.extend(head_entities.keys().copied());

    let mut changes: Vec<SemanticChangeRecord> = Vec::new();
    let mut lineage: Vec<LineageMatchRecord> = Vec::new();
    // Breaking candidates carry their change significance for deterministic
    // ordering; the consumer count is filled after one bounded relation scan.
    let mut breaking: Vec<(u16, BreakingCandidateRecord)> = Vec::new();
    let mut breaking_symbols: BTreeSet<SymbolId> = BTreeSet::new();

    for symbol in identities {
        control.check()?;
        match (base_entities.get(&symbol), head_entities.get(&symbol)) {
            (None, Some(head)) => {
                let kind = HistorySemanticChangeKind::Added;
                let change = SemanticChangeRecord {
                    kind,
                    symbol_id: symbol,
                    entity_kind: head.kind_label.clone(),
                    breaking_candidate: false,
                    significance: history_significance(kind, false),
                };
                emit_cycle_value(&mut changes, change, tracker, limiting_resources, control)?;
            }
            (Some(base), None) => {
                let kind = HistorySemanticChangeKind::Removed;
                let breaking_candidate = base.is_public;
                let significance = history_significance(kind, breaking_candidate);
                let change = SemanticChangeRecord {
                    kind,
                    symbol_id: symbol,
                    entity_kind: base.kind_label.clone(),
                    breaking_candidate,
                    significance,
                };
                emit_cycle_value(&mut changes, change, tracker, limiting_resources, control)?;
                if breaking_candidate {
                    breaking_symbols.insert(symbol);
                    breaking.push((
                        significance,
                        BreakingCandidateRecord {
                            symbol_id: symbol,
                            consumer_count: 0,
                            is_public_surface: true,
                            reason: "removed_public_surface".to_owned(),
                        },
                    ));
                }
            }
            (Some(base), Some(head)) => {
                // Identity preserved: an honest lineage match, never a rename.
                if lineage.len() < plan.max_results {
                    emit_cycle_value(
                        &mut lineage,
                        LineageMatchRecord {
                            base_symbol_id: symbol,
                            head_symbol_id: symbol,
                            confidence: 1_000,
                            is_rename: false,
                        },
                        tracker,
                        limiting_resources,
                        control,
                    )?;
                }
                // A kind or definition-span difference is a modification.
                if base.signature_differs(head) {
                    let kind = if base.kind_label != head.kind_label {
                        HistorySemanticChangeKind::SignatureModified
                    } else {
                        HistorySemanticChangeKind::Modified
                    };
                    let breaking_candidate = head.is_public;
                    let significance = history_significance(kind, breaking_candidate);
                    let change = SemanticChangeRecord {
                        kind,
                        symbol_id: symbol,
                        entity_kind: head.kind_label.clone(),
                        breaking_candidate,
                        significance,
                    };
                    emit_cycle_value(&mut changes, change, tracker, limiting_resources, control)?;
                    if breaking_candidate {
                        breaking_symbols.insert(symbol);
                        breaking.push((
                            significance,
                            BreakingCandidateRecord {
                                symbol_id: symbol,
                                consumer_count: 0,
                                is_public_surface: true,
                                reason: "modified_public_surface".to_owned(),
                            },
                        ));
                    }
                }
            }
            (None, None) => {}
        }
    }

    // Fill base-generation consumer counts for the breaking candidates.
    let incoming = count_history_incoming(
        base_document,
        &breaking_symbols,
        control,
        tracker,
        limiting_resources,
    )?;
    for (_, candidate) in &mut breaking {
        candidate.consumer_count = incoming.get(&candidate.symbol_id).copied().unwrap_or(0);
    }

    // Apply the optional change-kind filter.
    if !plan.change_kinds.is_empty() {
        changes.retain(|change| history_change_matches_filter(change.kind, &plan.change_kinds));
    }

    // Deterministic significance ordering under the result cap.
    changes.sort_by(|left, right| {
        right
            .significance
            .cmp(&left.significance)
            .then_with(|| left.symbol_id.cmp(&right.symbol_id))
    });
    if changes.len() > plan.max_results {
        record_limit(limiting_resources, QueryResource::Results)?;
    }
    changes.truncate(plan.max_results);

    breaking.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.symbol_id.cmp(&right.1.symbol_id))
    });
    let breaking_limit = plan.max_results.min(HISTORY_COMPARE_MAX_BREAKING);
    if breaking.len() > breaking_limit {
        record_limit(limiting_resources, QueryResource::Results)?;
    }
    let breaking_candidates: Vec<BreakingCandidateRecord> = breaking
        .into_iter()
        .take(breaking_limit)
        .map(|(_, candidate)| candidate)
        .collect();

    // Lineage was emitted in deterministic identity order; cap it.
    if lineage.len() > plan.max_results {
        record_limit(limiting_resources, QueryResource::Results)?;
    }
    lineage.truncate(plan.max_results);

    let coverage = if plan.base_generation == plan.explanation.generation {
        // Comparing a generation against itself is trivially complete.
        CoverageStatus::Complete
    } else if limits_optional_results(limiting_resources) {
        CoverageStatus::Sampled
    } else {
        // The entity diff is complete over both documents, but rename, move, and
        // architecture detection are documented out-of-scope bounds.
        CoverageStatus::Bounded
    };

    Ok(HistoryCompareAnalysis {
        coverage,
        changes,
        architecture_delta: HistoryArchitectureDelta {
            new_cross_service_edges: 0,
            removed_cross_service_edges: 0,
            new_boundaries: 0,
            removed_boundaries: 0,
        },
        breaking_candidates,
        lineage,
    })
}

/// Indexes one generation's entities by stable identity under the row budget.
fn history_entity_index(
    document: &NormalizedIrDocument,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<BTreeMap<SymbolId, HistoryEntityFingerprint>, QueryError> {
    let mut index: BTreeMap<SymbolId, HistoryEntityFingerprint> = BTreeMap::new();
    for entity in &document.entities {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        index.insert(entity.id, HistoryEntityFingerprint::from_entity(entity)?);
    }
    Ok(index)
}

/// Counts incoming entity-to-entity relations for the breaking symbols in base.
///
/// A relation whose object endpoint resolves to a breaking symbol contributes
/// one consumer; file endpoints and self-loops contribute nothing.
fn count_history_incoming(
    document: &NormalizedIrDocument,
    breaking_symbols: &BTreeSet<SymbolId>,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<BTreeMap<SymbolId, u32>, QueryError> {
    let mut incoming: BTreeMap<SymbolId, u32> = BTreeMap::new();
    if breaking_symbols.is_empty() {
        return Ok(incoming);
    }
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            break;
        }
        tracker.add_edges(1)?;
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        if !breaking_symbols.contains(&object) {
            continue;
        }
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        if subject == object {
            continue;
        }
        let count = incoming.entry(object).or_insert(0);
        *count = count.saturating_add(1);
    }
    Ok(incoming)
}

/// Returns the deterministic significance rank for one semantic change.
const fn history_significance(kind: HistorySemanticChangeKind, breaking_candidate: bool) -> u16 {
    let base = match kind {
        HistorySemanticChangeKind::Removed => 700,
        HistorySemanticChangeKind::SignatureModified => 600,
        HistorySemanticChangeKind::Modified => 400,
        HistorySemanticChangeKind::RelationChanged => 300,
        HistorySemanticChangeKind::Added => 200,
    };
    let boosted = if breaking_candidate { base + 300 } else { base };
    if boosted > 1_000 { 1_000 } else { boosted }
}

/// Returns whether one semantic change kind satisfies the change-kind filter.
fn history_change_matches_filter(
    kind: HistorySemanticChangeKind,
    filter: &BTreeSet<HistoryChangeKind>,
) -> bool {
    match kind {
        HistorySemanticChangeKind::Added
        | HistorySemanticChangeKind::Removed
        | HistorySemanticChangeKind::Modified => filter.contains(&HistoryChangeKind::Entities),
        HistorySemanticChangeKind::SignatureModified => {
            filter.contains(&HistoryChangeKind::Entities)
                || filter.contains(&HistoryChangeKind::Signatures)
        }
        HistorySemanticChangeKind::RelationChanged => {
            filter.contains(&HistoryChangeKind::Relations)
        }
    }
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
) -> Result<(Vec<CoverageRecord>, bool), QueryError> {
    let mut coverage = Vec::new();
    let mut truncated = false;
    for record in &document.coverage_records {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            truncated = true;
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
                truncated = true;
                break;
            }
            let bytes = serialized_size(record, u64::MAX, control)?;
            if !tracker.can_add(QueryResource::MemoryBytes, bytes) {
                record_limit(limiting_resources, QueryResource::MemoryBytes)?;
                truncated = true;
                break;
            }
            tracker.add_results(1)?;
            tracker.add_memory(bytes)?;
            try_push(&mut coverage, record.clone())?;
        }
    }
    Ok((coverage, truncated))
}

/// Finalizes successful supported execution from the resources observed by the
/// authoritative query producer.
fn authoritative_execution(limiting_resources: &[QueryResource]) -> ExecutionCompleteness {
    let Some((primary, additional)) = limiting_resources.split_first() else {
        return ExecutionCompleteness::complete();
    };
    ExecutionCompleteness::truncated(*primary, additional.iter().copied())
}

/// Finalizes a known unsupported semantic result without permitting an empty
/// unsupported cause list.
fn unsupported_execution(limiting_resources: &[QueryResource]) -> ExecutionCompleteness {
    debug_assert!(
        limiting_resources.contains(&QueryResource::Capability),
        "unsupported advanced execution records the capability boundary"
    );
    let Some((primary, additional)) = limiting_resources.split_first() else {
        return ExecutionCompleteness::unsupported_partial(
            QueryResource::Capability,
            std::iter::empty(),
        );
    };
    ExecutionCompleteness::unsupported_partial(*primary, additional.iter().copied())
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
            QueryResource::Depth | QueryResource::Paths | QueryResource::Capability => u64::MAX,
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
            QueryResource::Depth | QueryResource::Paths | QueryResource::Capability => {
                (0, u64::MAX)
            }
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

    use proptest::prelude::*;
    use proptest::test_runner::{RngAlgorithm, RngSeed};
    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_ids::SymbolId;
    use rootlight_ir::RelationPredicate;

    use super::*;
    use crate::model::{FlowTraceFrontier, FlowTracePath, QueryBudget, RelationFamily};

    struct CancelOnSerialize {
        cancellation: Cancellation,
    }

    impl Serialize for CancelOnSerialize {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let _won = self.cancellation.cancel(CancellationReason::ClientRequest);
            serializer.serialize_str("cancelled query output")
        }
    }

    #[test]
    fn counting_writer_observes_cancellation_during_serialization() {
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));

        let error = serialized_size(
            &CancelOnSerialize {
                cancellation: cancellation.clone(),
            },
            u64::MAX,
            &control,
        )
        .expect_err("mid-serialization cancellation must stop output measurement");

        assert!(matches!(
            error,
            QueryError::Cancelled(CancellationReason::ClientRequest)
        ));
    }

    #[test]
    fn resource_ledger_enforces_below_exact_and_above_runtime_boundaries() {
        let resources = [
            QueryResource::Rows,
            QueryResource::Edges,
            QueryResource::Results,
            QueryResource::SourceBytes,
            QueryResource::JsonBytes,
            QueryResource::Tokens,
            QueryResource::MemoryBytes,
        ];

        for resource in resources {
            let budget = match resource {
                QueryResource::Rows => QueryBudget::new().with_max_rows(2),
                QueryResource::Edges => QueryBudget::new().with_max_edges(2),
                QueryResource::Results => QueryBudget::new().with_max_results(2),
                QueryResource::SourceBytes => QueryBudget::new().with_max_source_bytes(2),
                QueryResource::JsonBytes => QueryBudget::new().with_max_json_bytes(2),
                QueryResource::Tokens => QueryBudget::new().with_max_tokens(2),
                QueryResource::MemoryBytes => QueryBudget::new().with_max_memory_bytes(2),
                QueryResource::Depth | QueryResource::Paths | QueryResource::Capability => {
                    unreachable!("untracked resource in the bounded test matrix")
                }
            };
            let tracker = UsageTracker::new(budget);

            tracker
                .require(resource, 1)
                .expect("the value below the runtime limit is admitted");
            tracker
                .require(resource, 2)
                .expect("the exact runtime limit is admitted");
            assert!(matches!(
                tracker.require(resource, 3),
                Err(QueryError::BudgetExceeded {
                    resource: observed,
                    limit: 2,
                }) if observed == resource
            ));
        }
    }

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
    ) -> (Vec<FlowTracePath>, FlowTraceFrontier, ExecutionCompleteness) {
        let budget = QueryBudget::new();
        let mut tracker = UsageTracker::new(budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, budget.max_duration);
        let (paths, frontier) = trace_flow(
            adjacency,
            from,
            to,
            max_depth,
            max_paths,
            &mut tracker,
            &mut limiting_resources,
            &control,
        )
        .expect("bounded trace succeeds");
        let execution = authoritative_execution(&limiting_resources);
        (paths, frontier, execution)
    }

    #[test]
    fn flow_trace_enumerates_outward_paths_with_correct_nodes_and_edges() {
        let (a, b, c) = (symbol(1), symbol(2), symbol(3));
        let adjacency = BTreeMap::from([
            (a, vec![edge(b, RelationFamily::Calls, 900)]),
            (b, vec![edge(c, RelationFamily::Calls, 800)]),
        ]);
        let (paths, frontier, execution) = run_trace(&adjacency, a, None, 3, 10);

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
        assert!(execution.is_complete());
    }

    #[test]
    fn flow_trace_returns_only_paths_that_reach_the_target() {
        let (a, b, c) = (symbol(1), symbol(2), symbol(3));
        let adjacency = BTreeMap::from([
            (a, vec![edge(b, RelationFamily::Calls, 900)]),
            (b, vec![edge(c, RelationFamily::Calls, 800)]),
        ]);
        let (paths, _, execution) = run_trace(&adjacency, a, Some(c), 3, 10);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].nodes, vec![a, b, c]);
        assert!(
            paths
                .iter()
                .all(|path| *path.nodes.last().expect("path has nodes") == c)
        );
        assert!(execution.is_complete());
    }

    #[test]
    fn flow_trace_marks_cycles_and_terminates() {
        let (a, b) = (symbol(1), symbol(2));
        let adjacency = BTreeMap::from([
            (a, vec![edge(b, RelationFamily::Calls, 500)]),
            (b, vec![edge(a, RelationFamily::Calls, 500)]),
        ]);
        let (paths, frontier, execution) = run_trace(&adjacency, a, None, 8, 100);

        assert_eq!(paths.len(), 2);
        let cyclic = paths
            .iter()
            .find(|path| path.cyclic)
            .expect("one cyclic path");
        assert_eq!(cyclic.nodes, vec![a, b, a]);
        assert!(paths.iter().any(|path| !path.cyclic));
        assert_eq!(frontier.reached_nodes, 2);
        assert!(!frontier.truncated);
        assert!(execution.is_complete());
    }

    #[test]
    fn flow_trace_honors_the_depth_bound_and_reports_a_boundary() {
        let (a, b, c, d) = (symbol(1), symbol(2), symbol(3), symbol(4));
        let adjacency = BTreeMap::from([
            (a, vec![edge(b, RelationFamily::Calls, 900)]),
            (b, vec![edge(c, RelationFamily::Calls, 900)]),
            (c, vec![edge(d, RelationFamily::Calls, 900)]),
        ]);
        let (paths, frontier, execution) = run_trace(&adjacency, a, None, 2, 100);

        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|path| path.nodes.len() <= 3));
        assert!(frontier.truncated);
        assert_eq!(frontier.reached_nodes, 3);
        assert_eq!(frontier.unresolved_boundaries, 1);
        assert!(execution.is_truncated());
        assert_eq!(execution.limiting_resources(), &[QueryResource::Depth]);
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
        let (paths, frontier, execution) = run_trace(&adjacency, a, None, 3, 1);

        assert_eq!(paths.len(), 1);
        assert!(frontier.truncated);
        assert!(execution.is_truncated());
        assert_eq!(execution.limiting_resources(), &[QueryResource::Paths]);
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
        run_detect_with_execution(adjacency, min_size, max_cycles, include_self_cycles).0
    }

    fn run_detect_with_execution(
        adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
        min_size: u8,
        max_cycles: usize,
        include_self_cycles: bool,
    ) -> (CycleDetection, ExecutionCompleteness) {
        let plan = cycle_plan(min_size, max_cycles, include_self_cycles);
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);
        let analysis = detect_cycles(
            adjacency,
            &plan,
            &mut tracker,
            &mut limiting_resources,
            &control,
        )
        .expect("bounded cycle detection succeeds");
        let execution = authoritative_execution(&limiting_resources);
        (analysis, execution)
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
        let ((components, cycles, breaks), execution) =
            run_detect_with_execution(&adjacency, 2, 1, false);
        assert_eq!(components.len(), 1);
        assert_eq!(cycles.len(), 1);
        assert_eq!(breaks.len(), 1);
        assert!(execution.is_truncated());
        assert_eq!(execution.limiting_resources(), &[QueryResource::Results]);
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
        run_dead_with_execution(graph, entry_points, max_candidates).0
    }

    fn run_dead_with_execution(
        graph: &DeadGraph,
        entry_points: &BTreeSet<SymbolId>,
        max_candidates: usize,
    ) -> (Vec<DeadCodeCandidate>, ExecutionCompleteness) {
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
        let candidates = detect_dead_candidates(
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
        .expect("bounded dead-code detection succeeds");
        let execution = authoritative_execution(&limiting_resources);
        (candidates, execution)
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
        let (candidates, execution) = run_dead_with_execution(&graph, &entry_points, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].symbol_id, c);
        assert!(execution.is_truncated());
        assert_eq!(execution.limiting_resources(), &[QueryResource::Results]);
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

    // -----------------------------------------------------------------
    // architecture.overview synthetic-document proofs
    // -----------------------------------------------------------------

    use crate::model::{ArchitectureOverviewPlan, ArchitectureOverviewView};
    use rootlight_ids::{ContentHash, FactId, FileId};
    use rootlight_ir::{
        AnalysisTier, Confidence, EntityKind, EntityRecord, EntityVisibility, EvidenceKind,
        FactEvidence, FileRecord, RelationEndpoint, RelationRecord, SourceRef, SourceSpan,
    };

    fn file_id(byte: u8) -> FileId {
        FileId::from_bytes([byte; 20])
    }

    fn overview_document() -> NormalizedIrDocument {
        NormalizedIrDocument::empty(
            RepositoryId::from_bytes([7; 16]),
            GenerationId::from_bytes([0; 20]),
        )
    }

    fn add_file(document: &mut NormalizedIrDocument, byte: u8, path: &str) {
        document.files.push(FileRecord {
            id: file_id(byte),
            repository: document.repository,
            generation: document.generation,
            path: path.to_owned(),
            path_locator: None,
            content_hash: ContentHash::from_bytes([byte; 32]),
            byte_length: 100,
            language: "rust".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: FactId::from_bytes([byte; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        });
    }

    fn add_entity(document: &mut NormalizedIrDocument, byte: u8, file_byte: u8, kind: EntityKind) {
        let source = SourceRef::new(
            document.repository,
            document.generation,
            SourceSpan::new(file_id(file_byte), 0, 10).expect("test span is ordered"),
            ContentHash::from_bytes([file_byte; 32]),
            None,
        );
        document.entities.push(EntityRecord {
            id: symbol(byte),
            repository: document.repository,
            generation: document.generation,
            kind,
            language: "rust".to_owned(),
            tier: AnalysisTier::TierD,
            canonical_name: format!("sym_{byte}"),
            display_name: format!("sym_{byte}"),
            qualified_name: format!("sym_{byte}"),
            container: None,
            visibility: EntityVisibility::Private,
            flags: Vec::new(),
            provenance: FactId::from_bytes([byte; 20]),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        });
    }

    fn add_relation(
        document: &mut NormalizedIrDocument,
        byte: u8,
        subject: RelationEndpoint,
        predicate: RelationPredicate,
        object: RelationEndpoint,
        confidence: u16,
    ) {
        document.relations.push(RelationRecord {
            id: FactId::from_bytes([byte; 20]),
            repository: document.repository,
            generation: document.generation,
            subject,
            predicate,
            object,
            confidence: Confidence::new(confidence).expect("test confidence is in range"),
            evidence_kind: EvidenceKind::Syntax,
            provenance: FactId::from_bytes([byte; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        });
    }

    fn add_contains(
        document: &mut NormalizedIrDocument,
        byte: u8,
        file_byte: u8,
        entity_byte: u8,
        confidence: u16,
    ) {
        add_relation(
            document,
            byte,
            RelationEndpoint::File(file_id(file_byte)),
            RelationPredicate::Contains,
            RelationEndpoint::Entity(symbol(entity_byte)),
            confidence,
        );
    }

    fn add_calls(
        document: &mut NormalizedIrDocument,
        byte: u8,
        from_byte: u8,
        to_byte: u8,
        confidence: u16,
    ) {
        add_relation(
            document,
            byte,
            RelationEndpoint::Entity(symbol(from_byte)),
            RelationPredicate::Calls,
            RelationEndpoint::Entity(symbol(to_byte)),
            confidence,
        );
    }

    fn add_refers(
        document: &mut NormalizedIrDocument,
        byte: u8,
        from_byte: u8,
        to_byte: u8,
        confidence: u16,
    ) {
        add_relation(
            document,
            byte,
            RelationEndpoint::Entity(symbol(from_byte)),
            RelationPredicate::RefersTo,
            RelationEndpoint::Entity(symbol(to_byte)),
            confidence,
        );
    }

    fn overview_plan(
        max_components: usize,
        include_edges: bool,
        min_confidence: u16,
        views: Vec<ArchitectureOverviewView>,
    ) -> ArchitectureOverviewPlan {
        ArchitectureOverviewPlan {
            views,
            min_confidence,
            max_components,
            include_edges,
            budget: QueryBudget::new(),
            explanation: PlanExplanation {
                generation: GenerationId::from_bytes([0; 20]),
                kind: PlanKind::ArchitectureOverview,
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

    fn run_overview(
        document: &NormalizedIrDocument,
        plan: &ArchitectureOverviewPlan,
    ) -> ArchitectureOverviewAnalysis {
        run_overview_with_execution(document, plan).0
    }

    fn run_overview_with_execution(
        document: &NormalizedIrDocument,
        plan: &ArchitectureOverviewPlan,
    ) -> (ArchitectureOverviewAnalysis, ExecutionCompleteness) {
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);
        let overview = build_architecture_overview(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )
        .expect("bounded architecture overview succeeds");
        let execution = authoritative_execution(&limiting_resources);
        (overview, execution)
    }

    #[test]
    fn architecture_overview_groups_symbols_into_file_components() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Struct);
        add_entity(&mut document, 13, 2, EntityKind::Function);
        add_contains(&mut document, 100, 1, 11, 800);
        add_contains(&mut document, 101, 1, 12, 600);
        add_contains(&mut document, 102, 2, 13, 700);

        let plan = overview_plan(50, true, 0, Vec::new());
        let overview = run_overview(&document, &plan);

        // Components are ordered by symbol count descending, so the two-symbol
        // file precedes the one-symbol file.
        assert_eq!(overview.components.len(), 2);
        assert_eq!(overview.components[0].id, file_id(1).to_string());
        assert_eq!(overview.components[0].kind, "file");
        assert_eq!(overview.components[0].name, "src/a.rs");
        assert_eq!(overview.components[0].symbol_count, 2);
        // Containment confidence is the strongest recorded `Contains` edge.
        assert_eq!(overview.components[0].confidence, 800);
        assert!(
            overview.components[0]
                .responsibility_evidence
                .contains(&"contains_symbols".to_owned())
        );
        assert_eq!(overview.components[1].id, file_id(2).to_string());
        assert_eq!(overview.components[1].name, "src/b.rs");
        assert_eq!(overview.components[1].symbol_count, 1);
        assert_eq!(overview.components[1].confidence, 700);

        assert!(overview.connections.is_empty());
        assert!(overview.hotspots.is_empty());
        assert!(overview.views.is_empty());
    }

    #[test]
    fn architecture_overview_aggregates_connections_between_components() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        add_entity(&mut document, 13, 2, EntityKind::Function);
        add_contains(&mut document, 100, 1, 11, 800);
        add_contains(&mut document, 101, 1, 12, 600);
        add_contains(&mut document, 102, 2, 13, 700);
        add_calls(&mut document, 110, 11, 13, 900);
        add_calls(&mut document, 111, 12, 13, 700);

        let plan = overview_plan(50, true, 0, Vec::new());
        let overview = run_overview(&document, &plan);

        // Both call edges aggregate into one typed connection from file 1 to
        // file 2 with the strongest confidence.
        assert_eq!(overview.connections.len(), 1);
        let connection = &overview.connections[0];
        assert_eq!(connection.from, file_id(1).to_string());
        assert_eq!(connection.to, file_id(2).to_string());
        assert_eq!(connection.kind, RelationFamily::Calls);
        assert_eq!(connection.weight, 2);
        assert_eq!(connection.confidence, 900);

        // Fan-in and fan-out rank the target above the source on tie-break.
        assert_eq!(overview.hotspots.len(), 2);
        assert_eq!(overview.hotspots[0].component_id, file_id(2).to_string());
        assert_eq!(overview.hotspots[0].fan_in, 1);
        assert_eq!(overview.hotspots[0].fan_out, 0);
        assert_eq!(overview.hotspots[0].score, 1_000);
        assert_eq!(overview.hotspots[1].component_id, file_id(1).to_string());
        assert_eq!(overview.hotspots[1].fan_in, 0);
        assert_eq!(overview.hotspots[1].fan_out, 1);
        assert_eq!(overview.hotspots[1].change_frequency, None);
        assert_eq!(overview.hotspots[1].complexity, None);
    }

    #[test]
    fn architecture_overview_separates_connections_by_relation_family() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 13, 2, EntityKind::Function);
        add_contains(&mut document, 100, 1, 11, 800);
        add_contains(&mut document, 102, 2, 13, 700);
        add_calls(&mut document, 110, 11, 13, 900);
        add_refers(&mut document, 111, 11, 13, 500);

        let plan = overview_plan(50, true, 0, Vec::new());
        let overview = run_overview(&document, &plan);

        // The call and reference edges form two distinct typed connections.
        assert_eq!(overview.connections.len(), 2);
        assert_eq!(overview.connections[0].kind, RelationFamily::Calls);
        assert_eq!(overview.connections[0].confidence, 900);
        assert_eq!(overview.connections[1].kind, RelationFamily::References);
        assert_eq!(overview.connections[1].confidence, 500);
    }

    #[test]
    fn architecture_overview_ranks_a_high_fan_in_hub_first() {
        let mut document = overview_document();
        for byte in 1..=4 {
            add_file(&mut document, byte, &format!("src/f{byte}.rs"));
            add_entity(&mut document, 10 + byte, byte, EntityKind::Function);
            add_contains(&mut document, 100 + byte, byte, 10 + byte, 800);
        }
        // Files 1, 3, and 4 all call into file 2, making file 2 a fan-in hub.
        add_calls(&mut document, 110, 11, 12, 900);
        add_calls(&mut document, 111, 13, 12, 900);
        add_calls(&mut document, 112, 14, 12, 900);

        let plan = overview_plan(50, true, 0, Vec::new());
        let overview = run_overview(&document, &plan);

        assert_eq!(overview.hotspots[0].component_id, file_id(2).to_string());
        assert_eq!(overview.hotspots[0].fan_in, 3);
        assert_eq!(overview.hotspots[0].fan_out, 0);
        assert_eq!(overview.hotspots[0].score, 1_000);
        // The three callers share the remaining score and order by identity.
        assert_eq!(overview.hotspots.len(), 4);
        assert_eq!(overview.hotspots[1].component_id, file_id(1).to_string());
        assert_eq!(overview.hotspots[2].component_id, file_id(3).to_string());
        assert_eq!(overview.hotspots[3].component_id, file_id(4).to_string());
        assert_eq!(overview.hotspots[1].score, 333);
    }

    #[test]
    fn architecture_overview_honors_the_max_components_cap() {
        let mut document = overview_document();
        for byte in 1..=3 {
            add_file(&mut document, byte, &format!("src/f{byte}.rs"));
            add_entity(&mut document, 10 + byte, byte, EntityKind::Function);
            add_contains(&mut document, 100 + byte, byte, 10 + byte, 800);
        }
        // A connection from the dropped file 3 must not survive the cap.
        add_calls(&mut document, 110, 13, 11, 900);

        let plan = overview_plan(2, true, 0, Vec::new());
        let (overview, execution) = run_overview_with_execution(&document, &plan);

        assert_eq!(overview.components.len(), 2);
        assert_eq!(overview.components[0].id, file_id(1).to_string());
        assert_eq!(overview.components[1].id, file_id(2).to_string());
        // File 3 is unreported, so its connection is excluded.
        assert!(overview.connections.is_empty());
        assert!(overview.hotspots.is_empty());
        assert!(execution.is_truncated());
        assert_eq!(execution.limiting_resources(), &[QueryResource::Results]);
    }

    #[test]
    fn architecture_overview_omits_edges_when_disabled() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 13, 2, EntityKind::Function);
        add_contains(&mut document, 100, 1, 11, 800);
        add_contains(&mut document, 102, 2, 13, 700);
        add_calls(&mut document, 110, 11, 13, 900);

        let plan = overview_plan(50, false, 0, Vec::new());
        let overview = run_overview(&document, &plan);

        assert_eq!(overview.components.len(), 2);
        assert!(overview.connections.is_empty());
        assert!(overview.hotspots.is_empty());
    }

    #[test]
    fn architecture_overview_honors_the_min_confidence_floor() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 13, 2, EntityKind::Function);
        add_contains(&mut document, 100, 1, 11, 800);
        add_contains(&mut document, 102, 2, 13, 700);
        add_calls(&mut document, 110, 11, 13, 400);

        let plan = overview_plan(50, true, 500, Vec::new());
        let overview = run_overview(&document, &plan);

        // The 400-confidence edge falls below the 500 floor.
        assert!(overview.connections.is_empty());
        assert!(overview.hotspots.is_empty());
    }

    #[test]
    fn architecture_overview_reports_requested_derived_view_metadata() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_contains(&mut document, 100, 1, 11, 800);

        let plan = overview_plan(50, true, 0, vec![ArchitectureOverviewView::Hotspots]);
        let overview = run_overview(&document, &plan);

        assert_eq!(overview.views.len(), 1);
        assert_eq!(overview.views[0].view, ArchitectureOverviewView::Hotspots);
        assert_eq!(overview.views[0].algorithm_version, "fan_in_out_v1");
    }

    #[test]
    fn architecture_overview_is_deterministic() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Struct);
        add_entity(&mut document, 13, 2, EntityKind::Function);
        add_contains(&mut document, 100, 1, 11, 800);
        add_contains(&mut document, 101, 1, 12, 600);
        add_contains(&mut document, 102, 2, 13, 700);
        add_calls(&mut document, 110, 11, 13, 900);
        add_calls(&mut document, 111, 12, 13, 700);

        let plan = overview_plan(50, true, 0, vec![ArchitectureOverviewView::Hotspots]);
        let first = run_overview(&document, &plan);
        let second = run_overview(&document, &plan);

        assert_eq!(first.components, second.components);
        assert_eq!(first.connections, second.connections);
        assert_eq!(first.hotspots, second.hotspots);
        assert_eq!(first.views, second.views);
    }

    // -----------------------------------------------------------------
    // tests.select synthetic-document proofs
    // -----------------------------------------------------------------

    use crate::model::{TestsSelectKind, TestsSelectPlan};

    fn tests_select_plan(
        seeds: BTreeSet<SymbolId>,
        test_kinds: Vec<TestsSelectKind>,
        max_tests: usize,
        include_commands: bool,
    ) -> TestsSelectPlan {
        TestsSelectPlan {
            seeds,
            test_kinds,
            max_tests,
            include_commands,
            budget: QueryBudget::new(),
            explanation: PlanExplanation {
                generation: GenerationId::from_bytes([0; 20]),
                kind: PlanKind::TestsSelect,
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

    fn run_tests_select(
        document: &NormalizedIrDocument,
        plan: &TestsSelectPlan,
    ) -> TestsSelectAnalysis {
        run_tests_select_with_execution(document, plan).0
    }

    fn run_tests_select_with_execution(
        document: &NormalizedIrDocument,
        plan: &TestsSelectPlan,
    ) -> (TestsSelectAnalysis, ExecutionCompleteness) {
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);
        let selection = build_tests_select(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )
        .expect("bounded tests select succeeds");
        let execution = authoritative_execution(&limiting_resources);
        (selection, execution)
    }

    #[test]
    fn tests_select_ranks_a_direct_edge_test_above_colocation() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/seed.rs");
        add_file(&mut document, 2, "src/test.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 21, 2, EntityKind::Test);
        add_entity(&mut document, 22, 1, EntityKind::Test);
        // Test 21 calls the seed directly; test 22 only shares the seed's file.
        add_calls(&mut document, 110, 21, 11, 900);

        let plan = tests_select_plan(BTreeSet::from([symbol(11)]), Vec::new(), 20, true);
        let selection = run_tests_select(&document, &plan);

        assert_eq!(selection.tests.len(), 2);
        // The direct-edge test ranks first with a confidence-weighted score.
        assert_eq!(selection.tests[0].test_id, symbol(21));
        assert_eq!(selection.tests[0].kind, TestsSelectKind::Unit);
        assert_eq!(selection.tests[0].score, 970);
        assert_eq!(selection.tests[0].path.as_deref(), Some("src/test.rs"));
        assert!(
            selection.tests[0]
                .why
                .contains(&"direct_test_edge".to_owned())
        );
        assert!(selection.tests[0].why.contains(&"via:calls".to_owned()));
        assert_eq!(
            selection.tests[0].command_hint.as_deref(),
            Some("test:unit")
        );
        assert_eq!(selection.tests[0].estimated_cost_ms, None);
        // The co-located test ranks second on the fixed co-location floor.
        assert_eq!(selection.tests[1].test_id, symbol(22));
        assert_eq!(selection.tests[1].score, 150);
        assert_eq!(selection.tests[1].path.as_deref(), Some("src/seed.rs"));
        assert!(
            selection.tests[1]
                .why
                .contains(&"shared_file_with_seed".to_owned())
        );
        // Both signals are reported used; history is never served in this slice.
        assert!(selection.coverage_strategy.direct_edges);
        assert!(selection.coverage_strategy.file_colocation_signals);
        assert!(!selection.coverage_strategy.transitive_signals);
        assert!(!selection.coverage_strategy.history_signals);
        // The seed is covered, so no gap is reported.
        assert!(selection.gaps.is_empty());
    }

    #[test]
    fn tests_select_uses_a_bounded_transitive_signal() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/t.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        add_entity(&mut document, 21, 2, EntityKind::Test);
        // test 21 -> intermediate 12 -> seed 11; weakest edge weights the path.
        add_calls(&mut document, 110, 21, 12, 800);
        add_calls(&mut document, 111, 12, 11, 600);

        let plan = tests_select_plan(BTreeSet::from([symbol(11)]), Vec::new(), 20, false);
        let selection = run_tests_select(&document, &plan);

        assert_eq!(selection.tests.len(), 1);
        assert_eq!(selection.tests[0].test_id, symbol(21));
        // Transitive band: 400 + 600 * 200 / 1000 = 520.
        assert_eq!(selection.tests[0].score, 520);
        assert!(
            selection.tests[0]
                .why
                .contains(&"transitive_dependency".to_owned())
        );
        assert_eq!(selection.tests[0].command_hint, None);
        assert!(!selection.coverage_strategy.direct_edges);
        assert!(selection.coverage_strategy.transitive_signals);
        assert!(!selection.coverage_strategy.file_colocation_signals);
        assert!(selection.gaps.is_empty());
    }

    #[test]
    fn tests_select_honors_the_max_tests_cap() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 21, 1, EntityKind::Test);
        add_entity(&mut document, 22, 1, EntityKind::Test);
        add_entity(&mut document, 23, 1, EntityKind::Test);

        let plan = tests_select_plan(BTreeSet::from([symbol(11)]), Vec::new(), 2, false);
        let (selection, execution) = run_tests_select_with_execution(&document, &plan);

        // All three tests are co-located; the cap keeps the lowest identities.
        assert_eq!(selection.tests.len(), 2);
        assert_eq!(selection.tests[0].test_id, symbol(21));
        assert_eq!(selection.tests[1].test_id, symbol(22));
        assert!(execution.is_truncated());
        assert_eq!(execution.limiting_resources(), &[QueryResource::Results]);
    }

    #[test]
    fn tests_select_reports_gaps_for_untested_seeds() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 2, EntityKind::Function);
        add_entity(&mut document, 21, 1, EntityKind::Test);
        add_calls(&mut document, 110, 21, 11, 900);

        let plan = tests_select_plan(
            BTreeSet::from([symbol(11), symbol(12)]),
            Vec::new(),
            20,
            false,
        );
        let selection = run_tests_select(&document, &plan);

        assert_eq!(selection.tests.len(), 1);
        assert_eq!(selection.tests[0].test_id, symbol(21));
        // Seed 12 has no related test, so it is reported as an honest gap.
        assert_eq!(selection.gaps.len(), 1);
        assert_eq!(selection.gaps[0].scope, symbol(12).to_string());
        assert_eq!(selection.gaps[0].reason, "no_related_test");
    }

    #[test]
    fn tests_select_filters_by_test_kind() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 21, 1, EntityKind::Test);
        add_calls(&mut document, 110, 21, 11, 900);

        // The lexical oracle reports every test as unit-level, so a unit filter
        // keeps it while an integration filter honestly selects nothing and
        // leaves the seed uncovered.
        let unit_plan = tests_select_plan(
            BTreeSet::from([symbol(11)]),
            vec![TestsSelectKind::Unit],
            20,
            false,
        );
        let unit_selection = run_tests_select(&document, &unit_plan);
        assert_eq!(unit_selection.tests.len(), 1);
        assert!(unit_selection.gaps.is_empty());

        let integration_plan = tests_select_plan(
            BTreeSet::from([symbol(11)]),
            vec![TestsSelectKind::Integration],
            20,
            false,
        );
        let integration_selection = run_tests_select(&document, &integration_plan);
        assert!(integration_selection.tests.is_empty());
        assert_eq!(integration_selection.gaps.len(), 1);
        assert_eq!(integration_selection.gaps[0].scope, symbol(11).to_string());
    }

    #[test]
    fn tests_select_is_deterministic() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/seed.rs");
        add_file(&mut document, 2, "src/test.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 2, EntityKind::Function);
        add_entity(&mut document, 21, 2, EntityKind::Test);
        add_entity(&mut document, 22, 1, EntityKind::Test);
        add_calls(&mut document, 110, 21, 11, 900);
        add_calls(&mut document, 111, 21, 12, 700);

        let plan = tests_select_plan(
            BTreeSet::from([symbol(11), symbol(12)]),
            Vec::new(),
            20,
            true,
        );
        let first = run_tests_select(&document, &plan);
        let second = run_tests_select(&document, &plan);

        assert_eq!(first.tests, second.tests);
        assert_eq!(first.gaps, second.gaps);
        assert_eq!(first.coverage_strategy, second.coverage_strategy);
    }

    fn add_public_entity(
        document: &mut NormalizedIrDocument,
        byte: u8,
        file_byte: u8,
        kind: EntityKind,
    ) {
        add_entity(document, byte, file_byte, kind);
        document
            .entities
            .last_mut()
            .expect("entity was just pushed")
            .visibility = EntityVisibility::Public;
    }

    fn change_impact_plan(
        changed_symbols: BTreeSet<SymbolId>,
        changed_paths: Vec<String>,
        max_depth: u8,
        min_confidence: u16,
        include_tests: bool,
        max_dependents: usize,
    ) -> ChangeImpactPlan {
        ChangeImpactPlan {
            changed_symbols,
            changed_paths,
            max_depth,
            min_confidence,
            include_tests,
            max_dependents,
            budget: QueryBudget::new(),
            explanation: PlanExplanation {
                generation: GenerationId::from_bytes([0; 20]),
                kind: PlanKind::ChangeImpact,
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

    fn run_change_impact(
        document: &NormalizedIrDocument,
        plan: &ChangeImpactPlan,
    ) -> ChangeImpactAnalysis {
        run_change_impact_with_execution(document, plan).0
    }

    fn run_change_impact_with_execution(
        document: &NormalizedIrDocument,
        plan: &ChangeImpactPlan,
    ) -> (ChangeImpactAnalysis, ExecutionCompleteness) {
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);
        let analysis = build_change_impact(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )
        .expect("bounded change impact succeeds");
        let execution = authoritative_execution(&limiting_resources);
        (analysis, execution)
    }

    #[test]
    fn change_impact_propagates_a_changed_symbol_to_dependents() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        add_entity(&mut document, 13, 1, EntityKind::Function);
        // 12 calls the changed 11 (distance 1); 13 calls 12 (distance 2).
        add_calls(&mut document, 110, 12, 11, 900);
        add_calls(&mut document, 111, 13, 12, 800);

        let plan = change_impact_plan(BTreeSet::from([symbol(11)]), Vec::new(), 3, 0, false, 500);
        let analysis = run_change_impact(&document, &plan);

        assert_eq!(analysis.resolved_changes.len(), 1);
        assert_eq!(analysis.resolved_changes[0].symbol_id, Some(symbol(11)));
        assert_eq!(
            analysis.resolved_changes[0].classification,
            ChangeImpactClassification::Body
        );

        assert_eq!(analysis.impacted.len(), 1);
        assert_eq!(analysis.impacted[0].source_index, 0);
        let dependents = &analysis.impacted[0].dependents;
        assert_eq!(dependents.len(), 2);
        // The direct caller ranks first at distance one with the edge confidence.
        assert_eq!(dependents[0].symbol_id, symbol(12));
        assert_eq!(dependents[0].distance, 1);
        assert_eq!(dependents[0].confidence, 900);
        assert_eq!(dependents[0].via, vec!["calls".to_owned()]);
        assert!(!dependents[0].is_public);
        // The transitive caller ranks second; confidence is the weakest edge.
        assert_eq!(dependents[1].symbol_id, symbol(13));
        assert_eq!(dependents[1].distance, 2);
        assert_eq!(dependents[1].confidence, 800);
        assert_eq!(
            dependents[1].via,
            vec!["calls".to_owned(), "calls".to_owned()]
        );

        // No public surface is touched, so the risk stays low with an honest fanout.
        assert_eq!(analysis.risk_summary.fanout, 2);
        assert!(!analysis.risk_summary.breaking_surface);
        assert_eq!(analysis.risk_summary.level, ChangeImpactRiskLevel::Low);
        assert!(analysis.risk_summary.dynamic_blind_spots);
        assert_eq!(analysis.risk_summary.coverage, CoverageStatus::Unknown);
        assert!(analysis.tests.is_empty());
    }

    #[test]
    fn change_impact_honors_the_max_depth_cap() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        add_entity(&mut document, 13, 1, EntityKind::Function);
        add_calls(&mut document, 110, 12, 11, 900);
        add_calls(&mut document, 111, 13, 12, 800);

        // A depth of one admits only the direct caller.
        let plan = change_impact_plan(BTreeSet::from([symbol(11)]), Vec::new(), 1, 0, false, 500);
        let (analysis, execution) = run_change_impact_with_execution(&document, &plan);

        let dependents = &analysis.impacted[0].dependents;
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].symbol_id, symbol(12));
        assert_eq!(dependents[0].distance, 1);
        assert_eq!(analysis.risk_summary.fanout, 1);
        assert!(execution.is_truncated());
        assert_eq!(execution.limiting_resources(), &[QueryResource::Depth]);
    }

    #[test]
    fn change_impact_honors_the_min_confidence_floor() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        // The only edge falls below the 500 confidence floor.
        add_calls(&mut document, 110, 12, 11, 400);

        let plan = change_impact_plan(BTreeSet::from([symbol(11)]), Vec::new(), 3, 500, false, 500);
        let analysis = run_change_impact(&document, &plan);

        assert_eq!(analysis.impacted.len(), 1);
        assert!(analysis.impacted[0].dependents.is_empty());
        assert_eq!(analysis.risk_summary.fanout, 0);
        assert_eq!(analysis.risk_summary.level, ChangeImpactRiskLevel::None);
        assert!(
            analysis
                .risk_summary
                .reasons
                .contains(&"no_measured_impact".to_owned())
        );
    }

    #[test]
    fn change_impact_resolves_an_explicit_path_to_its_declared_entities() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_contains(&mut document, 100, 1, 11, 800);

        let plan = change_impact_plan(
            BTreeSet::new(),
            vec!["src/a.rs".to_owned()],
            3,
            0,
            false,
            500,
        );
        let analysis = run_change_impact(&document, &plan);

        assert_eq!(analysis.resolved_changes.len(), 1);
        assert_eq!(analysis.resolved_changes[0].symbol_id, Some(symbol(11)));
        assert_eq!(analysis.resolved_changes[0].file_id, Some(file_id(1)));
        assert_eq!(
            analysis.resolved_changes[0].kind.as_deref(),
            Some("function")
        );
    }

    #[test]
    fn change_impact_reports_an_unknown_path_as_a_fully_unresolved_change() {
        let document = overview_document();
        let plan = change_impact_plan(
            BTreeSet::new(),
            vec!["src/missing.rs".to_owned()],
            3,
            0,
            false,
            500,
        );
        let analysis = run_change_impact(&document, &plan);

        // The unknown path still resolves to one honest fully-null change so the
        // caller's asserted change is not silently dropped.
        assert_eq!(analysis.resolved_changes.len(), 1);
        assert_eq!(analysis.resolved_changes[0].symbol_id, None);
        assert_eq!(analysis.resolved_changes[0].file_id, None);
        assert_eq!(
            analysis.resolved_changes[0].classification,
            ChangeImpactClassification::Body
        );
        // A file-only change has no symbol to propagate from.
        assert_eq!(analysis.impacted.len(), 1);
        assert!(analysis.impacted[0].dependents.is_empty());
    }

    #[test]
    fn change_impact_flags_a_public_dependent_as_breaking_surface() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_public_entity(&mut document, 12, 1, EntityKind::Function);
        add_calls(&mut document, 110, 12, 11, 900);

        let plan = change_impact_plan(BTreeSet::from([symbol(11)]), Vec::new(), 3, 0, false, 500);
        let analysis = run_change_impact(&document, &plan);

        let dependents = &analysis.impacted[0].dependents;
        assert_eq!(dependents.len(), 1);
        assert!(dependents[0].is_public);
        assert!(analysis.risk_summary.breaking_surface);
        assert_eq!(analysis.risk_summary.level, ChangeImpactRiskLevel::High);
        assert!(
            analysis
                .risk_summary
                .reasons
                .contains(&"public_surface_affected".to_owned())
        );
    }

    #[test]
    fn change_impact_is_deterministic() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        add_entity(&mut document, 13, 2, EntityKind::Function);
        add_entity(&mut document, 14, 2, EntityKind::Function);
        add_calls(&mut document, 110, 12, 11, 900);
        add_calls(&mut document, 111, 13, 11, 700);
        add_refers(&mut document, 112, 14, 13, 600);

        let plan = change_impact_plan(BTreeSet::from([symbol(11)]), Vec::new(), 3, 0, false, 500);
        let first = run_change_impact(&document, &plan);
        let second = run_change_impact(&document, &plan);

        assert_eq!(first.resolved_changes, second.resolved_changes);
        assert_eq!(first.impacted, second.impacted);
        assert_eq!(first.risk_summary, second.risk_summary);
    }

    fn plan_change_plan(
        objective: PlanChangeObjective,
        target_symbols: BTreeSet<SymbolId>,
        target_files: BTreeSet<FileId>,
        max_steps: usize,
    ) -> PlanChangePlan {
        PlanChangePlan {
            objective,
            objective_text: "change the selected targets".to_owned(),
            target_symbols,
            target_files,
            max_steps,
            max_depth: 3,
            max_dependents: 100,
            budget: QueryBudget::new(),
            explanation: PlanExplanation {
                generation: GenerationId::from_bytes([0; 20]),
                kind: PlanKind::PlanChange,
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

    fn run_plan_change(
        document: &NormalizedIrDocument,
        plan: &PlanChangePlan,
    ) -> PlanChangeAnalysis {
        run_plan_change_with_execution(document, plan).0
    }

    fn run_plan_change_with_execution(
        document: &NormalizedIrDocument,
        plan: &PlanChangePlan,
    ) -> (PlanChangeAnalysis, ExecutionCompleteness) {
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);
        let analysis = build_plan_change(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )
        .expect("bounded plan change succeeds");
        let execution = authoritative_execution(&limiting_resources);
        (analysis, execution)
    }

    #[test]
    fn plan_change_builds_ordered_steps_with_dependency_ordering() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        add_entity(&mut document, 13, 1, EntityKind::Function);
        // 12 calls the target 11 (distance 1); 13 calls 12 (distance 2).
        add_calls(&mut document, 110, 12, 11, 900);
        add_calls(&mut document, 111, 13, 12, 800);

        let mut plan = plan_change_plan(
            PlanChangeObjective::BugFix,
            BTreeSet::from([symbol(11)]),
            BTreeSet::new(),
            6,
        );
        plan.objective_text = "prevent duplicate publication".to_owned();
        let analysis = run_plan_change(&document, &plan);

        // A modification objective emits inspect, modify, update-dependents, and
        // run-tests steps in ordinal order.
        assert_eq!(analysis.plan.len(), 4);
        for (index, step) in analysis.plan.iter().enumerate() {
            assert_eq!(step.step, u8::try_from(index + 1).expect("ordinal fits"));
            // Every dependency references an earlier ordinal.
            assert!(step.depends_on.iter().all(|dep| *dep < step.step));
            assert!(!step.action.is_empty());
        }
        // The inspect step targets the resolved symbol.
        assert_eq!(analysis.plan[0].targets, vec![symbol(11)]);
        assert!(analysis.plan[0].depends_on.is_empty());
        assert!(
            analysis.plan[0]
                .action
                .contains("prevent duplicate publication")
        );
        // The modify step depends on inspect.
        assert_eq!(analysis.plan[1].depends_on, vec![1]);
        assert_eq!(analysis.plan[1].targets, vec![symbol(11)]);
        // The update-dependents step carries the direct dependent and depends on modify.
        assert_eq!(analysis.plan[2].depends_on, vec![2]);
        assert_eq!(analysis.plan[2].targets, vec![symbol(12)]);
        // The run-tests step depends on modify and update-dependents.
        assert_eq!(analysis.plan[3].depends_on, vec![2, 3]);

        // The impact summary counts the target plus its two reached dependents.
        assert_eq!(analysis.affected_scope.affected_symbols, 3);
        assert_eq!(analysis.affected_scope.affected_files, 1);
        assert!(!analysis.affected_scope.touches_public_surface);
        assert_eq!(
            analysis.affected_scope.risk_level,
            ChangeImpactRiskLevel::Low
        );

        // The context pack carries the affected symbols and their declaring file.
        assert_eq!(
            analysis.context_pack_request.symbols,
            vec![symbol(11), symbol(12), symbol(13)]
        );
        assert_eq!(analysis.context_pack_request.files, vec![file_id(1)]);
    }

    #[test]
    fn plan_change_honors_the_max_steps_cap() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        add_calls(&mut document, 110, 12, 11, 900);

        let plan = plan_change_plan(
            PlanChangeObjective::BugFix,
            BTreeSet::from([symbol(11)]),
            BTreeSet::new(),
            2,
        );
        let (analysis, execution) = run_plan_change_with_execution(&document, &plan);

        assert_eq!(analysis.plan.len(), 2);
        assert_eq!(analysis.plan[0].step, 1);
        assert_eq!(analysis.plan[1].step, 2);
        // Truncation keeps every dependency reference valid.
        assert!(analysis.plan[1].depends_on.iter().all(|dep| *dep <= 2));
        assert!(execution.is_truncated());
        assert_eq!(execution.limiting_resources(), &[QueryResource::Results]);
    }

    #[test]
    fn plan_change_flags_public_surface_risk_and_decision() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_public_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        add_calls(&mut document, 110, 12, 11, 900);

        let plan = plan_change_plan(
            PlanChangeObjective::BugFix,
            BTreeSet::from([symbol(11)]),
            BTreeSet::new(),
            6,
        );
        let analysis = run_plan_change(&document, &plan);

        assert!(analysis.affected_scope.touches_public_surface);
        assert_eq!(
            analysis.affected_scope.risk_level,
            ChangeImpactRiskLevel::High
        );
        // A public-surface change adds a confirmation step and an open decision.
        assert_eq!(analysis.plan.len(), 5);
        assert_eq!(analysis.plan[4].step, 5);
        assert!(
            analysis
                .open_decisions
                .iter()
                .any(|decision| decision.question == "confirm_public_surface_change")
        );
    }

    #[test]
    fn plan_change_resolves_a_file_target_to_its_declared_entities() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_contains(&mut document, 100, 1, 11, 800);

        let plan = plan_change_plan(
            PlanChangeObjective::Review,
            BTreeSet::new(),
            BTreeSet::from([file_id(1)]),
            6,
        );
        let analysis = run_plan_change(&document, &plan);

        // The file target expands to the entity it declares, which becomes the
        // inspect step target and the context-pack symbol.
        assert_eq!(analysis.plan[0].targets, vec![symbol(11)]);
        assert_eq!(analysis.context_pack_request.symbols, vec![symbol(11)]);
        assert_eq!(analysis.context_pack_request.files, vec![file_id(1)]);
        assert_eq!(analysis.affected_scope.affected_symbols, 1);
    }

    #[test]
    fn plan_change_explanation_objective_emits_read_only_steps() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);

        let plan = plan_change_plan(
            PlanChangeObjective::Explanation,
            BTreeSet::from([symbol(11)]),
            BTreeSet::new(),
            6,
        );
        let analysis = run_plan_change(&document, &plan);

        assert_eq!(analysis.plan.len(), 3);
        // No modification step is emitted for a read-only objective.
        assert!(
            analysis
                .plan
                .iter()
                .all(|step| !step.action.contains("Apply") && !step.action.contains("Migrate"))
        );
        assert!(analysis.open_decisions.is_empty());
    }

    #[test]
    fn plan_change_is_deterministic() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        add_entity(&mut document, 13, 2, EntityKind::Function);
        add_calls(&mut document, 110, 12, 11, 900);
        add_calls(&mut document, 111, 13, 11, 700);

        let plan = plan_change_plan(
            PlanChangeObjective::Refactor,
            BTreeSet::from([symbol(11)]),
            BTreeSet::new(),
            6,
        );
        let first = run_plan_change(&document, &plan);
        let second = run_plan_change(&document, &plan);

        assert_eq!(first.plan, second.plan);
        assert_eq!(first.affected_scope, second.affected_scope);
        assert_eq!(first.open_decisions, second.open_decisions);
        assert_eq!(first.context_pack_request, second.context_pack_request);
        assert_eq!(first.test_plan, second.test_plan);
    }

    fn history_document(gen_byte: u8) -> NormalizedIrDocument {
        NormalizedIrDocument::empty(
            RepositoryId::from_bytes([7; 16]),
            GenerationId::from_bytes([gen_byte; 20]),
        )
    }

    fn history_generation(gen_byte: u8) -> GenerationId {
        GenerationId::from_bytes([gen_byte; 20])
    }

    fn history_compare_plan(
        base_generation: GenerationId,
        head_generation: GenerationId,
        change_kinds: BTreeSet<HistoryChangeKind>,
        max_results: usize,
    ) -> HistoryComparePlan {
        HistoryComparePlan {
            base_generation,
            change_kinds,
            max_results,
            budget: QueryBudget::new(),
            explanation: PlanExplanation {
                generation: head_generation,
                kind: PlanKind::HistoryCompare,
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

    fn run_history_compare(
        base: &NormalizedIrDocument,
        head: &NormalizedIrDocument,
        plan: &HistoryComparePlan,
    ) -> HistoryCompareAnalysis {
        run_history_compare_with_execution(base, head, plan).0
    }

    fn run_history_compare_with_execution(
        base: &NormalizedIrDocument,
        head: &NormalizedIrDocument,
        plan: &HistoryComparePlan,
    ) -> (HistoryCompareAnalysis, ExecutionCompleteness) {
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);
        let analysis = build_history_compare(
            base,
            head,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )
        .expect("bounded history compare succeeds");
        let execution = authoritative_execution(&limiting_resources);
        (analysis, execution)
    }

    #[test]
    fn history_compare_detects_added_removed_and_preserved_entities() {
        let mut base = history_document(1);
        add_file(&mut base, 1, "src/a.rs");
        add_entity(&mut base, 11, 1, EntityKind::Function);
        add_entity(&mut base, 12, 1, EntityKind::Function);

        let mut head = history_document(2);
        add_file(&mut head, 1, "src/a.rs");
        add_entity(&mut head, 11, 1, EntityKind::Function);
        add_entity(&mut head, 13, 1, EntityKind::Function);

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        let analysis = run_history_compare(&base, &head, &plan);

        // The removed entity (significance 700) ranks before the addition (200).
        assert_eq!(analysis.changes.len(), 2);
        assert_eq!(analysis.changes[0].kind, HistorySemanticChangeKind::Removed);
        assert_eq!(analysis.changes[0].symbol_id, symbol(12));
        assert_eq!(analysis.changes[0].significance, 700);
        assert!(!analysis.changes[0].breaking_candidate);
        assert_eq!(analysis.changes[1].kind, HistorySemanticChangeKind::Added);
        assert_eq!(analysis.changes[1].symbol_id, symbol(13));
        assert_eq!(analysis.changes[1].significance, 200);

        // The preserved identity forms an honest lineage match, never a rename.
        assert_eq!(analysis.lineage.len(), 1);
        assert_eq!(analysis.lineage[0].base_symbol_id, symbol(11));
        assert_eq!(analysis.lineage[0].head_symbol_id, symbol(11));
        assert_eq!(analysis.lineage[0].confidence, 1_000);
        assert!(!analysis.lineage[0].is_rename);

        // No public surface and no service model: no breaking candidates, zeros.
        assert!(analysis.breaking_candidates.is_empty());
        assert_eq!(analysis.architecture_delta.new_cross_service_edges, 0);
        assert_eq!(analysis.architecture_delta.removed_cross_service_edges, 0);
        assert_eq!(analysis.architecture_delta.new_boundaries, 0);
        assert_eq!(analysis.architecture_delta.removed_boundaries, 0);
        assert_eq!(analysis.coverage, CoverageStatus::Bounded);
    }

    #[test]
    fn history_compare_flags_a_public_removal_as_breaking_with_consumer_count() {
        let mut base = history_document(1);
        add_file(&mut base, 1, "src/a.rs");
        add_public_entity(&mut base, 21, 1, EntityKind::Function);
        add_entity(&mut base, 22, 1, EntityKind::Function);
        // 22 calls 21, so the removed public symbol has one base consumer.
        add_calls(&mut base, 110, 22, 21, 900);

        let mut head = history_document(2);
        add_file(&mut head, 1, "src/a.rs");
        add_entity(&mut head, 22, 1, EntityKind::Function);

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        let analysis = run_history_compare(&base, &head, &plan);

        assert_eq!(analysis.changes[0].kind, HistorySemanticChangeKind::Removed);
        assert_eq!(analysis.changes[0].symbol_id, symbol(21));
        assert!(analysis.changes[0].breaking_candidate);
        assert_eq!(analysis.changes[0].significance, 1_000);

        assert_eq!(analysis.breaking_candidates.len(), 1);
        assert_eq!(analysis.breaking_candidates[0].symbol_id, symbol(21));
        assert_eq!(analysis.breaking_candidates[0].consumer_count, 1);
        assert!(analysis.breaking_candidates[0].is_public_surface);
        assert_eq!(
            analysis.breaking_candidates[0].reason,
            "removed_public_surface"
        );
    }

    #[test]
    fn history_compare_detects_a_kind_change_as_signature_modified() {
        let mut base = history_document(1);
        add_file(&mut base, 1, "src/a.rs");
        add_entity(&mut base, 31, 1, EntityKind::Function);

        let mut head = history_document(2);
        add_file(&mut head, 1, "src/a.rs");
        add_entity(&mut head, 31, 1, EntityKind::Struct);

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        let analysis = run_history_compare(&base, &head, &plan);

        // The identity is preserved as lineage, but the kind change is a
        // signature-level modification.
        assert_eq!(analysis.changes.len(), 1);
        assert_eq!(
            analysis.changes[0].kind,
            HistorySemanticChangeKind::SignatureModified
        );
        assert_eq!(analysis.changes[0].symbol_id, symbol(31));
        assert_eq!(analysis.changes[0].significance, 600);
        assert!(!analysis.changes[0].breaking_candidate);
        assert_eq!(analysis.lineage.len(), 1);
        assert_eq!(analysis.lineage[0].base_symbol_id, symbol(31));
    }

    #[test]
    fn history_compare_reports_an_empty_complete_comparison_when_base_equals_head() {
        let mut document = history_document(1);
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(1),
            BTreeSet::new(),
            100,
        );
        let (analysis, execution) = run_history_compare_with_execution(&document, &document, &plan);

        assert!(analysis.changes.is_empty());
        assert!(analysis.breaking_candidates.is_empty());
        assert_eq!(analysis.architecture_delta.new_cross_service_edges, 0);
        assert_eq!(analysis.architecture_delta.removed_cross_service_edges, 0);
        assert_eq!(analysis.architecture_delta.new_boundaries, 0);
        assert_eq!(analysis.architecture_delta.removed_boundaries, 0);
        assert_eq!(analysis.coverage, CoverageStatus::Complete);
        assert!(execution.is_complete());
        // Both identities survive as honest, non-rename lineage matches.
        assert_eq!(analysis.lineage.len(), 2);
        assert!(analysis.lineage.iter().all(|lineage| {
            lineage.base_symbol_id == lineage.head_symbol_id
                && !lineage.is_rename
                && lineage.confidence == 1_000
        }));
    }

    #[test]
    fn history_compare_honors_the_change_kind_filter() {
        let mut base = history_document(1);
        add_file(&mut base, 1, "src/a.rs");
        add_entity(&mut base, 12, 1, EntityKind::Function);

        let mut head = history_document(2);
        add_file(&mut head, 1, "src/a.rs");
        add_entity(&mut head, 13, 1, EntityKind::Function);

        let entities = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::from([HistoryChangeKind::Entities]),
            100,
        );
        let analysis = run_history_compare(&base, &head, &entities);
        assert_eq!(analysis.changes.len(), 2);

        // A signatures-only filter admits no entity addition or removal.
        let signatures = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::from([HistoryChangeKind::Signatures]),
            100,
        );
        let analysis = run_history_compare(&base, &head, &signatures);
        assert!(analysis.changes.is_empty());
    }

    #[test]
    fn history_compare_honors_the_max_results_cap() {
        let base = history_document(1);
        let mut head = history_document(2);
        add_file(&mut head, 1, "src/a.rs");
        for byte in [41u8, 42, 43, 44] {
            add_entity(&mut head, byte, 1, EntityKind::Function);
        }

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            2,
        );
        let (analysis, execution) = run_history_compare_with_execution(&base, &head, &plan);

        assert_eq!(analysis.changes.len(), 2);
        assert!(
            analysis
                .changes
                .iter()
                .all(|change| change.kind == HistorySemanticChangeKind::Added)
        );
        assert_ne!(analysis.coverage, CoverageStatus::Complete);
        assert!(execution.is_truncated());
        assert_eq!(execution.limiting_resources(), &[QueryResource::Results]);
    }

    #[test]
    fn history_compare_is_deterministic() {
        let mut base = history_document(1);
        add_file(&mut base, 1, "src/a.rs");
        add_public_entity(&mut base, 21, 1, EntityKind::Function);
        add_entity(&mut base, 22, 1, EntityKind::Function);
        add_calls(&mut base, 110, 22, 21, 900);

        let mut head = history_document(2);
        add_file(&mut head, 1, "src/a.rs");
        add_entity(&mut head, 22, 1, EntityKind::Function);
        add_entity(&mut head, 23, 1, EntityKind::Function);

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        let first = run_history_compare(&base, &head, &plan);
        let second = run_history_compare(&base, &head, &plan);

        assert_eq!(first.changes, second.changes);
        assert_eq!(first.breaking_candidates, second.breaking_candidates);
        assert_eq!(first.lineage, second.lineage);
        assert_eq!(first.architecture_delta, second.architecture_delta);
        assert_eq!(first.coverage, second.coverage);
    }

    // -----------------------------------------------------------------
    // query.advanced synthetic-document proofs
    // -----------------------------------------------------------------

    use crate::model::{
        ADVANCED_MAX_DEPTH, ADVANCED_MAX_ESTIMATED_COST, ADVANCED_MAX_RESULTS,
        ADVANCED_MAX_TRAVERSAL, AdvancedOperator, AdvancedRelationKind, AdvancedTraverseDirection,
    };

    fn advanced_document() -> NormalizedIrDocument {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Struct);
        add_entity(&mut document, 13, 2, EntityKind::Function);
        document
    }

    fn advanced_plan(ast: AdvancedAstNode, explain: bool, max_rows: usize) -> AdvancedQueryPlan {
        let (operators, depth) = ast.derive_plan_shape();
        let estimated_cost =
            AdvancedQueryPlan::validate(&operators, max_rows, ADVANCED_MAX_TRAVERSAL, depth)
                .expect("test advanced plan is valid");
        AdvancedQueryPlan {
            ast,
            operators,
            max_rows,
            page_offset: 0,
            max_traversal: ADVANCED_MAX_TRAVERSAL,
            depth,
            estimated_cost,
            explain,
            budget: QueryBudget::new(),
            explanation: PlanExplanation {
                generation: GenerationId::from_bytes([0; 20]),
                kind: PlanKind::QueryAdvanced,
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

    fn run_advanced(document: &NormalizedIrDocument, plan: &AdvancedQueryPlan) -> AdvancedBuild {
        try_run_advanced(document, plan).expect("bounded advanced query succeeds")
    }

    fn try_run_advanced(
        document: &NormalizedIrDocument,
        plan: &AdvancedQueryPlan,
    ) -> Result<AdvancedBuild, QueryError> {
        let mut tracker = UsageTracker::new(advanced_runtime_budget(plan)?);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);
        build_advanced_query(
            document,
            plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )
    }

    fn scan_functions() -> AdvancedAstNode {
        AdvancedAstNode::Scan {
            entity: AdvancedEntityKind::Function,
            filter: None,
        }
    }

    #[test]
    fn advanced_result_materialization_enforces_the_exact_result_budget() {
        let document = advanced_document();

        let mut below = advanced_plan(scan_functions(), false, 2);
        below.budget = QueryBudget::new().with_max_results(1);
        assert!(matches!(
            try_run_advanced(&document, &below),
            Err(QueryError::BudgetExceeded {
                resource: QueryResource::Results,
                limit: 1,
            })
        ));

        let mut exact = advanced_plan(scan_functions(), false, 2);
        exact.budget = QueryBudget::new().with_max_results(2);
        let built =
            try_run_advanced(&document, &exact).expect("two result slots admit two function rows");
        assert_eq!(built.rows.len(), 2);
    }

    #[test]
    fn advanced_execution_observes_an_expired_duration_budget_before_work() {
        let document = advanced_document();
        let mut plan = advanced_plan(scan_functions(), false, 2);
        plan.budget = QueryBudget::new().with_max_duration(Duration::from_millis(1));
        let mut tracker =
            UsageTracker::new(advanced_runtime_budget(&plan).expect("runtime budget is valid"));
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);
        std::thread::sleep(Duration::from_millis(5));

        assert!(matches!(
            build_advanced_query(
                &document,
                &plan,
                &control,
                &mut tracker,
                &mut limiting_resources,
            ),
            Err(QueryError::Cancelled(CancellationReason::DeadlineExceeded))
        ));
        assert_eq!(tracker.rows, 0);
        assert_eq!(tracker.edges, 0);
        assert_eq!(tracker.results, 0);
    }

    #[test]
    fn advanced_scan_filter_project_limit_returns_exact_rows() {
        let document = advanced_document();
        // Scan functions (sym_11, sym_13), keep only sym_11, project id+name.
        let ast = AdvancedAstNode::Limit {
            input: Box::new(AdvancedAstNode::Project {
                input: Box::new(AdvancedAstNode::Filter {
                    input: Box::new(scan_functions()),
                    predicate: AdvancedPredicate::Equals {
                        field: "name".to_owned(),
                        value: AdvancedValue::Text("sym_11".to_owned()),
                    },
                }),
                columns: vec!["id".to_owned(), "name".to_owned()],
            }),
            max_rows: 10,
        };
        let plan = advanced_plan(ast, false, 100);
        let built = run_advanced(&document, &plan);

        assert_eq!(built.completeness, AdvancedCompleteness::Complete);
        assert!(built.execution.is_complete());
        assert_eq!(
            built.columns,
            vec![
                AdvancedColumnSchema {
                    name: "id".to_owned(),
                    column_type: AdvancedColumnType::SymbolId
                },
                AdvancedColumnSchema {
                    name: "name".to_owned(),
                    column_type: AdvancedColumnType::Text
                },
            ]
        );
        assert_eq!(built.rows.len(), 1);
        assert_eq!(
            built.rows[0]["id"],
            serde_json::Value::String(symbol(11).to_string())
        );
        assert_eq!(
            built.rows[0]["name"],
            serde_json::Value::String("sym_11".to_owned())
        );
    }

    #[test]
    fn advanced_scan_returns_matching_entities_in_identity_order() {
        let document = advanced_document();
        let ast = AdvancedAstNode::Limit {
            input: Box::new(scan_functions()),
            max_rows: 100,
        };
        let plan = advanced_plan(ast, false, 100);
        let built = run_advanced(&document, &plan);

        assert_eq!(built.completeness, AdvancedCompleteness::Complete);
        // The default scan schema is always non-empty.
        assert_eq!(built.columns.len(), 4);
        assert_eq!(built.rows.len(), 2);
        // Deterministic identity order: sym_11 precedes sym_13.
        assert_eq!(
            built.rows[0]["id"],
            serde_json::Value::String(symbol(11).to_string())
        );
        assert_eq!(
            built.rows[1]["id"],
            serde_json::Value::String(symbol(13).to_string())
        );
        assert_eq!(
            built.rows[0]["kind"],
            serde_json::Value::String("function".to_owned())
        );
        assert_eq!(
            built.rows[0]["path"],
            serde_json::Value::String("src/a.rs".to_owned())
        );
    }

    #[test]
    fn advanced_explain_returns_a_plan_without_rows() {
        let document = advanced_document();
        let plan = advanced_plan(scan_functions(), true, 100);
        let built = run_advanced(&document, &plan);

        assert_eq!(built.completeness, AdvancedCompleteness::Complete);
        assert!(built.rows.is_empty());
        assert!(!built.columns.is_empty());
        assert!(built.plan.operators.contains(&"Scan".to_owned()));
        assert!(built.plan.estimated_cost > 0);
        assert!(!built.plan.applied_limits.is_empty());
    }

    #[test]
    fn advanced_aggregate_groups_and_counts_rows_deterministically() {
        let document = advanced_document();
        let ast = AdvancedAstNode::Aggregate {
            input: Box::new(scan_functions()),
            group_by: vec!["kind".to_owned()],
            aggregations: vec![AdvancedAggregateFunction::Count],
        };
        let plan = advanced_plan(ast, false, 100);
        let built = run_advanced(&document, &plan);

        assert_eq!(built.completeness, AdvancedCompleteness::Complete);
        assert!(built.execution.is_complete());
        assert_eq!(
            built.columns,
            vec![
                AdvancedColumnSchema {
                    name: "kind".to_owned(),
                    column_type: AdvancedColumnType::Text,
                },
                AdvancedColumnSchema {
                    name: "count".to_owned(),
                    column_type: AdvancedColumnType::Integer,
                },
            ]
        );
        assert_eq!(built.rows.len(), 1);
        assert_eq!(built.rows[0]["kind"], serde_json::json!("function"));
        assert_eq!(built.rows[0]["count"], serde_json::json!(2));
    }

    #[test]
    fn advanced_join_uses_a_bounded_shared_key_and_stable_row_order() {
        let document = advanced_document();
        let ast = AdvancedAstNode::Join {
            left: Box::new(scan_functions()),
            right: Box::new(scan_functions()),
            on: "id".to_owned(),
        };
        let plan = advanced_plan(ast, false, 100);
        let built = run_advanced(&document, &plan);

        assert_eq!(built.completeness, AdvancedCompleteness::Complete);
        assert!(built.execution.is_complete());
        assert_eq!(built.rows.len(), 2);
        assert_eq!(
            built.rows[0]["id"],
            serde_json::Value::String(symbol(11).to_string())
        );
        assert_eq!(
            built.rows[1]["id"],
            serde_json::Value::String(symbol(13).to_string())
        );
    }

    #[test]
    fn advanced_traverse_follows_bounded_edges_in_stable_order() {
        let mut document = advanced_document();
        add_calls(&mut document, 1, 11, 13, 900);
        add_calls(&mut document, 2, 13, 12, 900);
        let ast = AdvancedAstNode::Traverse {
            seed: Some(symbol(11)),
            seed_from: None,
            relation: AdvancedRelationKind::Calls,
            direction: AdvancedTraverseDirection::Outbound,
            max_depth: Some(2),
        };
        let plan = advanced_plan(ast, false, 100);
        let built = run_advanced(&document, &plan);

        assert_eq!(built.completeness, AdvancedCompleteness::Complete);
        assert!(built.execution.is_complete());
        assert_eq!(built.rows.len(), 2);
        assert_eq!(
            built.rows[0]["source"],
            serde_json::Value::String(symbol(11).to_string())
        );
        assert_eq!(
            built.rows[0]["target"],
            serde_json::Value::String(symbol(13).to_string())
        );
        assert_eq!(
            built.rows[1]["target"],
            serde_json::Value::String(symbol(12).to_string())
        );
        assert_eq!(built.rows[0]["relation"], serde_json::json!("calls"));
    }

    #[test]
    fn advanced_seed_from_traverse_remains_honestly_unsupported() {
        let document = advanced_document();
        let ast = AdvancedAstNode::Traverse {
            seed: None,
            seed_from: Some("id".to_owned()),
            relation: AdvancedRelationKind::Calls,
            direction: AdvancedTraverseDirection::Outbound,
            max_depth: Some(1),
        };
        let plan = advanced_plan(ast, false, 100);
        let built = run_advanced(&document, &plan);

        assert_eq!(built.completeness, AdvancedCompleteness::Unsupported);
        assert!(built.execution.is_unsupported_partial());
        assert_eq!(
            built.execution.limiting_resources(),
            &[QueryResource::Capability]
        );
    }

    #[test]
    fn advanced_result_cap_emits_a_page_continuation() {
        let document = advanced_document();
        let plan = advanced_plan(scan_functions(), false, 1);
        let built = run_advanced(&document, &plan);

        assert_eq!(built.completeness, AdvancedCompleteness::Paged);
        assert!(built.execution.is_truncated());
        assert_eq!(built.execution.limiting_resources(), &[QueryResource::Rows]);
        assert_eq!(built.rows.len(), 1);
        assert_eq!(built.next_page_offset, Some(1));
    }

    #[test]
    fn advanced_pages_concatenate_without_duplicates_or_omissions() {
        let document = advanced_document();
        let baseline = run_advanced(
            &document,
            &advanced_plan(scan_functions(), false, ADVANCED_MAX_RESULTS),
        );
        let mut observed = Vec::new();
        let mut offset = 0_usize;

        loop {
            let mut plan = advanced_plan(scan_functions(), false, 1);
            plan.page_offset = offset;
            let page = run_advanced(&document, &plan);
            observed.extend(page.rows);
            match page.next_page_offset {
                Some(next) => {
                    assert_eq!(page.completeness, AdvancedCompleteness::Paged);
                    assert!(page.execution.is_truncated());
                    offset = usize::try_from(next).expect("test offset fits");
                }
                None => {
                    assert_eq!(page.completeness, AdvancedCompleteness::Complete);
                    assert!(page.execution.is_complete());
                    break;
                }
            }
        }

        assert_eq!(observed, baseline.rows);
        let identities: std::collections::BTreeSet<_> = observed
            .iter()
            .map(|row| row["id"].as_str().expect("id is text"))
            .collect();
        assert_eq!(
            identities.len(),
            observed.len(),
            "pages contain no duplicates"
        );
    }

    fn advanced_service_campaign_cases() -> u32 {
        std::env::var("ROOTLIGHT_ADVANCED_GATE_CASES")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|cases| (1..=4_096).contains(cases))
            .unwrap_or(48)
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: advanced_service_campaign_cases(),
            max_shrink_iters: 256,
            failure_persistence: None,
            rng_algorithm: RngAlgorithm::ChaCha,
            rng_seed: RngSeed::Fixed(202_607_220_041),
            ..ProptestConfig::default()
        })]

        #[test]
        fn advanced_scan_order_and_page_concatenation_ignore_insertion_order(
            entity_ids in prop::collection::btree_set(20_u8..=60, 0..=12),
            page_size in 1_usize..=8,
        ) {
            let mut ascending = advanced_document();
            for id in &entity_ids {
                add_entity(&mut ascending, *id, 1, EntityKind::Function);
            }
            let mut descending = advanced_document();
            for id in entity_ids.iter().rev() {
                add_entity(&mut descending, *id, 1, EntityKind::Function);
            }

            let baseline = run_advanced(
                &ascending,
                &advanced_plan(scan_functions(), false, ADVANCED_MAX_RESULTS),
            );
            let reordered = run_advanced(
                &descending,
                &advanced_plan(scan_functions(), false, ADVANCED_MAX_RESULTS),
            );
            prop_assert_eq!(&reordered.rows, &baseline.rows);

            let mut observed = Vec::new();
            let mut offset = 0_usize;
            loop {
                let mut plan = advanced_plan(scan_functions(), false, page_size);
                plan.page_offset = offset;
                let page = run_advanced(&ascending, &plan);
                observed.extend(page.rows);
                match page.next_page_offset {
                    Some(next) => {
                        offset = usize::try_from(next).expect("test offset fits");
                    }
                    None => break,
                }
            }

            prop_assert_eq!(observed, baseline.rows);
        }
    }

    #[test]
    fn advanced_sort_orders_rows_deterministically() {
        let document = advanced_document();
        let ast = AdvancedAstNode::Sort {
            input: Box::new(scan_functions()),
            by: vec![AdvancedSortKey {
                field: "name".to_owned(),
                descending: true,
            }],
        };
        let plan = advanced_plan(ast, false, 100);
        let built = run_advanced(&document, &plan);

        assert_eq!(built.rows.len(), 2);
        // Descending by name: sym_13 precedes sym_11.
        assert_eq!(
            built.rows[0]["name"],
            serde_json::Value::String("sym_13".to_owned())
        );
        assert_eq!(
            built.rows[1]["name"],
            serde_json::Value::String("sym_11".to_owned())
        );
    }

    #[test]
    fn advanced_validate_rejects_an_excessive_cost_estimate() {
        // Many expensive operators scaled by the maximum row count push the
        // static cost past the hard ceiling, so the plan is rejected.
        let operators = vec![AdvancedOperator::Join; 200];
        let result = AdvancedQueryPlan::validate(
            &operators,
            ADVANCED_MAX_RESULTS,
            ADVANCED_MAX_TRAVERSAL,
            2,
        );
        assert!(result.is_err());
        // A single cheap scan stays well under the ceiling.
        let cheap =
            AdvancedQueryPlan::validate(&[AdvancedOperator::Scan], 100, ADVANCED_MAX_TRAVERSAL, 1)
                .expect("cheap plan is admitted");
        assert!(cheap <= ADVANCED_MAX_ESTIMATED_COST);
    }

    #[test]
    fn advanced_validate_rejects_excessive_depth() {
        let result = AdvancedQueryPlan::validate(
            &[AdvancedOperator::Scan],
            100,
            ADVANCED_MAX_TRAVERSAL,
            ADVANCED_MAX_DEPTH + 1,
        );
        assert!(result.is_err());
    }

    #[test]
    fn advanced_admits_cost_honors_the_client_limit() {
        assert!(AdvancedQueryPlan::admits_cost(100, None));
        assert!(AdvancedQueryPlan::admits_cost(100, Some(100)));
        assert!(AdvancedQueryPlan::admits_cost(100, Some(101)));
        assert!(!AdvancedQueryPlan::admits_cost(101, Some(100)));
    }

    #[test]
    fn advanced_query_is_deterministic() {
        let document = advanced_document();
        let ast = AdvancedAstNode::Limit {
            input: Box::new(AdvancedAstNode::Project {
                input: Box::new(scan_functions()),
                columns: vec!["id".to_owned(), "name".to_owned(), "path".to_owned()],
            }),
            max_rows: 50,
        };
        let plan = advanced_plan(ast, false, 100);
        let first = run_advanced(&document, &plan);
        let second = run_advanced(&document, &plan);
        assert_eq!(first.columns, second.columns);
        assert_eq!(first.rows, second.rows);
        assert_eq!(first.completeness, second.completeness);
    }

    fn advanced_integer_rows(values: &[i64]) -> AdvancedRowSet {
        let rows = values
            .iter()
            .map(|value| BTreeMap::from([("id".to_owned(), AdvancedValue::Integer(*value))]))
            .collect();
        AdvancedRowSet {
            columns: vec![AdvancedColumnSchema {
                name: "id".to_owned(),
                column_type: AdvancedColumnType::Integer,
            }],
            rows,
        }
    }

    fn advanced_text_rows(values: &[&str]) -> AdvancedRowSet {
        let rows = values
            .iter()
            .map(|value| {
                BTreeMap::from([("id".to_owned(), AdvancedValue::Text((*value).to_owned()))])
            })
            .collect();
        AdvancedRowSet {
            columns: vec![AdvancedColumnSchema {
                name: "id".to_owned(),
                column_type: AdvancedColumnType::Text,
            }],
            rows,
        }
    }

    fn advanced_file_paths(document: &NormalizedIrDocument) -> BTreeMap<FileId, String> {
        document
            .files
            .iter()
            .map(|file| (file.id, file.path.clone()))
            .collect()
    }

    fn expect_advanced_error(
        result: Result<AdvancedRowSet, QueryError>,
        message: &str,
    ) -> QueryError {
        match result {
            Ok(_) => panic!("{message}"),
            Err(error) => error,
        }
    }

    #[test]
    fn advanced_scan_checks_the_row_budget_at_the_exact_boundary() {
        let document = advanced_document();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));

        let mut below = UsageTracker::new(QueryBudget::new().with_max_rows(1));
        let error = expect_advanced_error(
            eval_advanced_scan(
                &document,
                AdvancedEntityKind::Function,
                None,
                &advanced_file_paths(&document),
                &control,
                &mut below,
            ),
            "the second matching entity exceeds a one-row budget",
        );
        assert!(matches!(
            error,
            QueryError::BudgetExceeded {
                resource: QueryResource::Rows,
                limit: 1,
            }
        ));
        assert_eq!(below.rows, 1);

        let mut exact = UsageTracker::new(QueryBudget::new().with_max_rows(2));
        let rows = eval_advanced_scan(
            &document,
            AdvancedEntityKind::Function,
            None,
            &advanced_file_paths(&document),
            &control,
            &mut exact,
        )
        .expect("two matching entities fit the exact row budget");
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(exact.rows, 2);
    }

    #[test]
    fn advanced_join_charges_every_candidate_pair() {
        let left = advanced_integer_rows(&[1, 2]);
        let right = advanced_integer_rows(&[1, 2, 3]);
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));

        let mut below = UsageTracker::new(QueryBudget::new().with_max_edges(5));
        let error = expect_advanced_error(
            advanced_join_rows(
                advanced_integer_rows(&[1, 2]),
                advanced_integer_rows(&[1, 2, 3]),
                "id",
                &control,
                &mut below,
            ),
            "a two-by-three join requires six candidate checks",
        );
        assert!(matches!(
            error,
            QueryError::BudgetExceeded {
                resource: QueryResource::Edges,
                limit: 5,
            }
        ));
        assert_eq!(below.edges, 5);

        let mut exact = UsageTracker::new(QueryBudget::new().with_max_edges(6));
        let joined = advanced_join_rows(left, right, "id", &control, &mut exact)
            .expect("the exact candidate-pair budget is sufficient");
        assert_eq!(exact.edges, 6);
        assert_eq!(joined.rows.len(), 2);
    }

    #[test]
    fn advanced_join_materialization_is_memory_bounded() {
        let joined_row =
            BTreeMap::from([("id".to_owned(), AdvancedValue::Text("alpha".to_owned()))]);
        let required =
            advanced_row_owned_bytes(&joined_row).expect("joined row size is representable");
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));

        let mut below = UsageTracker::new(QueryBudget::new().with_max_memory_bytes(required - 1));
        let error = expect_advanced_error(
            advanced_join_rows(
                advanced_text_rows(&["alpha"]),
                advanced_text_rows(&["alpha"]),
                "id",
                &control,
                &mut below,
            ),
            "join materialization must respect the memory ledger",
        );
        assert!(matches!(
            error,
            QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit,
            } if limit == required - 1
        ));
        assert_eq!(below.memory_bytes, 0);

        let mut exact = UsageTracker::new(QueryBudget::new().with_max_memory_bytes(required));
        let joined = advanced_join_rows(
            advanced_text_rows(&["alpha"]),
            advanced_text_rows(&["alpha"]),
            "id",
            &control,
            &mut exact,
        )
        .expect("the exact joined-row memory budget is sufficient");
        assert_eq!(exact.memory_bytes, required);
        assert_eq!(joined.rows, vec![joined_row]);
    }

    #[test]
    fn advanced_aggregate_charges_each_input_row_at_the_boundary() {
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));

        let mut below = UsageTracker::new(QueryBudget::new().with_max_edges(2));
        let error = expect_advanced_error(
            advanced_aggregate_rows(
                advanced_integer_rows(&[1, 2, 3]),
                &["id".to_owned()],
                &[AdvancedAggregateFunction::Count],
                &control,
                &mut below,
            ),
            "three input rows exceed a two-edge aggregation budget",
        );
        assert!(matches!(
            error,
            QueryError::BudgetExceeded {
                resource: QueryResource::Edges,
                limit: 2,
            }
        ));
        assert_eq!(below.edges, 2);

        let mut exact = UsageTracker::new(QueryBudget::new().with_max_edges(3));
        let groups = advanced_aggregate_rows(
            advanced_integer_rows(&[1, 2, 3]),
            &["id".to_owned()],
            &[AdvancedAggregateFunction::Count],
            &control,
            &mut exact,
        )
        .expect("the exact input-row budget is sufficient");
        assert_eq!(exact.edges, 3);
        assert_eq!(groups.rows.len(), 3);
    }

    #[test]
    fn advanced_aggregate_groups_and_output_are_memory_bounded() {
        let key = [AdvancedValue::Text("alpha".to_owned())];
        let group_bytes = advanced_group_owned_bytes(&key, 1).expect("group size is representable");
        let output_row = BTreeMap::from([
            ("count".to_owned(), AdvancedValue::Integer(1)),
            ("id".to_owned(), AdvancedValue::Text("alpha".to_owned())),
        ]);
        let output_bytes =
            advanced_row_owned_bytes(&output_row).expect("output row size is representable");
        let required = group_bytes + output_bytes;
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));
        let group_by = ["id".to_owned()];
        let aggregations = [AdvancedAggregateFunction::Count];

        let mut below = UsageTracker::new(QueryBudget::new().with_max_memory_bytes(required - 1));
        let error = expect_advanced_error(
            advanced_aggregate_rows(
                advanced_text_rows(&["alpha"]),
                &group_by,
                &aggregations,
                &control,
                &mut below,
            ),
            "aggregate output must respect the memory ledger",
        );
        assert!(matches!(
            error,
            QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit,
            } if limit == required - 1
        ));
        assert_eq!(below.memory_bytes, group_bytes);

        let mut exact = UsageTracker::new(QueryBudget::new().with_max_memory_bytes(required));
        let rows = advanced_aggregate_rows(
            advanced_text_rows(&["alpha"]),
            &group_by,
            &aggregations,
            &control,
            &mut exact,
        )
        .expect("the exact group and output memory budget is sufficient");
        assert_eq!(exact.memory_bytes, required);
        assert_eq!(rows.rows, vec![output_row]);
    }

    #[test]
    fn advanced_traversal_charges_indexing_and_expansion_edges() {
        let mut document = advanced_document();
        add_calls(&mut document, 1, 11, 13, 900);
        add_calls(&mut document, 2, 13, 12, 900);
        let relation_count =
            u64::try_from(document.relations.len()).expect("fixture relation count fits u64");
        let exact_edge_count = relation_count + 2;
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));

        let mut below = UsageTracker::new(QueryBudget::new().with_max_edges(exact_edge_count - 1));
        let error = expect_advanced_error(
            advanced_traverse_rows(
                &document,
                symbol(11),
                AdvancedRelationKind::Calls,
                AdvancedTraverseDirection::Outbound,
                2,
                &control,
                &mut below,
            ),
            "indexing plus two expansions exceed the lower budget",
        );
        assert!(matches!(
            error,
            QueryError::BudgetExceeded {
                resource: QueryResource::Edges,
                limit,
            } if limit == exact_edge_count - 1
        ));

        let mut exact = UsageTracker::new(QueryBudget::new().with_max_edges(exact_edge_count));
        let traversed = advanced_traverse_rows(
            &document,
            symbol(11),
            AdvancedRelationKind::Calls,
            AdvancedTraverseDirection::Outbound,
            2,
            &control,
            &mut exact,
        )
        .expect("the exact indexing and expansion budget is sufficient");
        assert_eq!(exact.edges, exact_edge_count);
        assert_eq!(traversed.rows.len(), 2);
    }

    #[test]
    fn advanced_long_operators_reject_precancelled_work_without_results() {
        let mut document = advanced_document();
        add_calls(&mut document, 1, 11, 13, 900);
        let file_paths = advanced_file_paths(&document);
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));

        let mut scan_tracker = UsageTracker::new(QueryBudget::new());
        assert!(matches!(
            eval_advanced_scan(
                &document,
                AdvancedEntityKind::Function,
                None,
                &file_paths,
                &control,
                &mut scan_tracker,
            ),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert_eq!(scan_tracker.rows, 0);

        let mut join_tracker = UsageTracker::new(QueryBudget::new());
        assert!(matches!(
            advanced_join_rows(
                advanced_integer_rows(&[1]),
                advanced_integer_rows(&[1]),
                "id",
                &control,
                &mut join_tracker,
            ),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert_eq!(join_tracker.edges, 0);

        let mut aggregate_tracker = UsageTracker::new(QueryBudget::new());
        assert!(matches!(
            advanced_aggregate_rows(
                advanced_integer_rows(&[1]),
                &["id".to_owned()],
                &[AdvancedAggregateFunction::Count],
                &control,
                &mut aggregate_tracker,
            ),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert_eq!(aggregate_tracker.edges, 0);

        let mut traversal_tracker = UsageTracker::new(QueryBudget::new());
        assert!(matches!(
            advanced_traverse_rows(
                &document,
                symbol(11),
                AdvancedRelationKind::Calls,
                AdvancedTraverseDirection::Outbound,
                1,
                &control,
                &mut traversal_tracker,
            ),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert_eq!(traversal_tracker.edges, 0);
    }

    #[test]
    fn advanced_transform_stages_reject_precancelled_work() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));
        let predicate = AdvancedPredicate::Equals {
            field: "id".to_owned(),
            value: AdvancedValue::Integer(1),
        };

        assert!(matches!(
            advanced_filter_rows(advanced_integer_rows(&[1, 2]), &predicate, &control),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert!(matches!(
            advanced_project_rows(advanced_integer_rows(&[1, 2]), &["id".to_owned()], &control,),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert!(matches!(
            advanced_limit_rows(advanced_integer_rows(&[1, 2]), 1, &control),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));

        let mut rows = advanced_integer_rows(&[2, 1]).rows;
        let mut tracker = UsageTracker::new(QueryBudget::new());
        assert!(matches!(
            advanced_sort_rows(
                &mut rows,
                &[AdvancedSortKey {
                    field: "id".to_owned(),
                    descending: false,
                }],
                &control,
                &mut tracker,
            ),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert_eq!(tracker.memory_bytes, 0);
    }

    #[test]
    fn advanced_sort_workspace_is_memory_bounded_at_the_exact_boundary() {
        let row_count = 3;
        let required =
            advanced_sort_workspace_bytes(row_count).expect("workspace size is representable");
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));
        let keys = [AdvancedSortKey {
            field: "id".to_owned(),
            descending: false,
        }];

        let mut below_rows = advanced_integer_rows(&[3, 1, 2]).rows;
        let mut below = UsageTracker::new(QueryBudget::new().with_max_memory_bytes(required - 1));
        assert!(matches!(
            advanced_sort_rows(&mut below_rows, &keys, &control, &mut below),
            Err(QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit,
            }) if limit == required - 1
        ));
        assert_eq!(below.memory_bytes, 0);

        let mut exact_rows = advanced_integer_rows(&[3, 1, 2]).rows;
        let mut exact = UsageTracker::new(QueryBudget::new().with_max_memory_bytes(required));
        advanced_sort_rows(&mut exact_rows, &keys, &control, &mut exact)
            .expect("the exact sort workspace budget is sufficient");
        assert_eq!(exact.memory_bytes, required);
        assert_eq!(
            exact_rows
                .iter()
                .map(|row| row["id"].clone())
                .collect::<Vec<_>>(),
            vec![
                AdvancedValue::Integer(1),
                AdvancedValue::Integer(2),
                AdvancedValue::Integer(3),
            ]
        );
    }

    #[test]
    fn advanced_declared_work_cap_limits_join_fanout() {
        let document = advanced_document();
        let ast = AdvancedAstNode::Join {
            left: Box::new(scan_functions()),
            right: Box::new(scan_functions()),
            on: "id".to_owned(),
        };

        let mut below = advanced_plan(ast.clone(), false, 100);
        below.max_traversal = 3;
        let error = match try_run_advanced(&document, &below) {
            Ok(_) => panic!("the global advanced edge-work cap must reject the fourth join pair"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            QueryError::BudgetExceeded {
                resource: QueryResource::Edges,
                limit: 3,
            }
        ));

        let mut exact = advanced_plan(ast, false, 100);
        exact.max_traversal = 4;
        let built =
            try_run_advanced(&document, &exact).expect("four join pairs fit the exact work cap");
        assert_eq!(built.rows.len(), 2);
    }

    #[test]
    fn advanced_traversal_depth_is_rejected_instead_of_clamped() {
        let document = advanced_document();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));

        for depth in [0, 6] {
            let mut tracker = UsageTracker::new(QueryBudget::new());
            let error = expect_advanced_error(
                advanced_traverse_rows(
                    &document,
                    symbol(11),
                    AdvancedRelationKind::Calls,
                    AdvancedTraverseDirection::Outbound,
                    depth,
                    &control,
                    &mut tracker,
                ),
                "invalid traversal depth must be rejected",
            );
            assert!(matches!(
                error,
                QueryError::PlanRejected {
                    resource: QueryResource::Depth,
                }
            ));
            assert_eq!(tracker.edges, 0);
        }

        for depth in [None, Some(1), Some(5)] {
            let ast = AdvancedAstNode::Traverse {
                seed: Some(symbol(11)),
                seed_from: None,
                relation: AdvancedRelationKind::Calls,
                direction: AdvancedTraverseDirection::Outbound,
                max_depth: depth,
            };
            assert!(advanced_traversal_depths_valid(&ast));
        }
        for depth in [Some(0), Some(6)] {
            let ast = AdvancedAstNode::Traverse {
                seed: Some(symbol(11)),
                seed_from: None,
                relation: AdvancedRelationKind::Calls,
                direction: AdvancedTraverseDirection::Outbound,
                max_depth: depth,
            };
            assert!(!advanced_traversal_depths_valid(&ast));
        }
    }

    #[test]
    fn advanced_static_cost_accepts_the_exact_ceiling() {
        let exact = vec![AdvancedOperator::Join; 200];
        assert_eq!(
            AdvancedQueryPlan::validate(&exact, 999, ADVANCED_MAX_TRAVERSAL, 2)
                .expect("the exact static cost ceiling is accepted"),
            ADVANCED_MAX_ESTIMATED_COST
        );

        let mut excessive = exact;
        excessive.push(AdvancedOperator::Limit);
        assert!(AdvancedQueryPlan::validate(&excessive, 999, ADVANCED_MAX_TRAVERSAL, 2).is_err());
    }

    #[test]
    fn advanced_work_limit_rejects_zero_and_accepts_the_hard_ceiling() {
        let operators = [AdvancedOperator::Scan];
        assert!(AdvancedQueryPlan::validate(&operators, 1, 0, 1).is_err());
        assert!(AdvancedQueryPlan::validate(&operators, 1, ADVANCED_MAX_TRAVERSAL, 1).is_ok());
        assert!(AdvancedQueryPlan::validate(&operators, 1, ADVANCED_MAX_TRAVERSAL + 1, 1).is_err());
    }
}
