use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
    io, mem,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{ContentHash, FactId, FileId, GenerationId, SymbolId, content_hash};
use rootlight_ir::{
    AnalysisTier, ContainerRef, CoverageRecord, CoverageScope, CoverageStatus, EntityFlag,
    EntityKind, EntityVisibility, FactDomain, NormalizedIrDocument, OccurrenceTarget, ProducerKind,
    RelationEndpoint, RelationPredicate, SourceRef,
};
use rootlight_search::{
    LexicalSearch, SearchBudget, SearchRequest, validate_search_request_with_languages,
};
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
    AdvancedSortKey, AdvancedTraverseDirection, AdvancedValue, AnalysisScope,
    ArchitectureCommunity, ArchitectureComponent, ArchitectureConnection, ArchitectureCyclesPlan,
    ArchitectureCyclesProjection, ArchitectureCyclesResult, ArchitectureHotspot,
    ArchitectureOverviewDerivedView, ArchitectureOverviewDetail, ArchitectureOverviewPlan,
    ArchitectureOverviewResult, ArchitectureOverviewView, BreakingCandidateRecord,
    ChangeImpactClassification, ChangeImpactPlan, ChangeImpactRelationPolicy, ChangeImpactResult,
    ChangeImpactRiskLevel, ChangeImpactRiskSummary, ChangeImpactTestCandidate, CodeDeadBlindSpot,
    CodeDeadEntryPointPolicy, CodeDeadEntryPointSummary, CodeDeadPlan, CodeDeadResult,
    CodeDeadSuppressionRule, CodeLocatePlan, CodeLocateResult, CycleBreak, CycleComponent,
    CyclePath, CycleProjectionLevel, CycleRankBy, DeadCodeCandidate, DeadCodeClassification,
    DeadCodeReachabilitySummary, ExecutionCompleteness, FlowTraceEdge, FlowTraceFrontier,
    FlowTracePath, FlowTracePlan, FlowTraceProjection, FlowTraceResult, HistoryArchitectureDelta,
    HistoryChangeKind, HistoryComparePlan, HistoryCompareResult, HistoryCompareScope,
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
        search_budget: SearchBudget,
        budget: QueryBudget,
    ) -> Result<CodeLocatePlan, QueryError> {
        self.plan_code_locate_with_languages(
            query,
            mode,
            Vec::new(),
            max_results,
            page_offset,
            search_budget,
            budget,
        )
    }

    /// Builds a deterministic bounded `code.locate` plan over a language union.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid language filter, budget, result
    /// limit, arithmetic overflow, or a conservative estimate that cannot be admitted.
    #[expect(
        clippy::too_many_arguments,
        reason = "the language union is an independent bounded locate dimension"
    )]
    pub fn plan_code_locate_with_languages(
        &self,
        query: String,
        mode: LocateMode,
        languages: Vec<String>,
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
        validate_search_request_with_languages(&request, &languages, search_budget)?;
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
            languages,
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
        let outcome = self.search.search_with_language_filter_and_stats(
            &request,
            &plan.languages,
            plan.search_budget,
            cancellation,
        )?;
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
                if occurrence_targets_symbol(occurrence, plan.symbol) {
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
        let mut saw_non_exact_relation = false;

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
                let candidates = relation_candidates(document, relation, &plan.seeds, effective);
                if !candidates.is_empty()
                    && relation.predicate == RelationPredicate::DispatchCandidate
                {
                    saw_non_exact_relation = true;
                }
                let confidence = effective_relation_confidence(document, relation);
                for (seed, direction, target) in candidates {
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
        let coverage =
            repository_coverage_summary(document, &control, &mut tracker, &mut limiting_resources)?;
        scan_truncated |= coverage.truncated;
        let coverage_complete = relationship_families_are_complete(&plan.families, &coverage);

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
            exact: !scan_truncated && coverage_complete && !saw_non_exact_relation,
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
        families: Vec<RelationFamily>,
        min_confidence: u16,
        min_size: u8,
        max_cycles: usize,
        include_self_cycles: bool,
        budget: QueryBudget,
    ) -> Result<ArchitectureCyclesPlan, QueryError> {
        self.plan_architecture_cycles_with_options(
            families,
            None,
            CycleProjectionLevel::Symbol,
            min_confidence,
            min_size,
            max_cycles,
            include_self_cycles,
            CycleRankBy::Size,
            budget,
        )
    }

    /// Builds a deterministic bounded cycle plan with scope, aggregation, and ranking.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid scope, budget, relation family,
    /// projection level, confidence, component-size, or cycle bound.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded cycle-analysis dimension"
    )]
    pub fn plan_architecture_cycles_with_options(
        &self,
        mut families: Vec<RelationFamily>,
        scope: Option<AnalysisScope>,
        level: CycleProjectionLevel,
        min_confidence: u16,
        min_size: u8,
        max_cycles: usize,
        include_self_cycles: bool,
        rank_by: CycleRankBy,
        budget: QueryBudget,
    ) -> Result<ArchitectureCyclesPlan, QueryError> {
        budget.validate()?;
        validate_analysis_scope(scope.as_ref())?;
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
            scope,
            level,
            min_confidence,
            min_size,
            max_cycles,
            include_self_cycles,
            rank_by,
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

        let (adjacency, omitted_nodes) = build_cycle_adjacency(
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
                level: plan.level,
                min_confidence: plan.min_confidence,
                rank_by: plan.rank_by,
                omitted_nodes,
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
        self.plan_code_dead_with_options(
            entry_point_policy,
            BTreeSet::new(),
            None,
            include_exported,
            include_tests,
            min_confidence,
            max_candidates,
            budget,
        )
    }

    /// Builds a deterministic bounded dead-code plan with a typed entry model and scope.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid scope, explicit-entry, confidence,
    /// candidate, budget, arithmetic, or admission bounds.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded reachability-analysis dimension"
    )]
    pub fn plan_code_dead_with_options(
        &self,
        entry_point_policy: CodeDeadEntryPointPolicy,
        explicit_entry_points: BTreeSet<SymbolId>,
        scope: Option<AnalysisScope>,
        include_exported: bool,
        include_tests: bool,
        min_confidence: u16,
        max_candidates: usize,
        budget: QueryBudget,
    ) -> Result<CodeDeadPlan, QueryError> {
        budget.validate()?;
        validate_analysis_scope(scope.as_ref())?;
        if explicit_entry_points.len() > 64
            || matches!(entry_point_policy, CodeDeadEntryPointPolicy::Explicit)
                == explicit_entry_points.is_empty()
        {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
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
            explicit_entry_points,
            scope,
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
            coverage_caveats: analysis.coverage_caveats,
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
        views: Vec<ArchitectureOverviewView>,
        min_confidence: u16,
        max_components: usize,
        include_edges: bool,
        budget: QueryBudget,
    ) -> Result<ArchitectureOverviewPlan, QueryError> {
        self.plan_architecture_overview_with_options(
            views,
            None,
            ArchitectureOverviewDetail::Standard,
            min_confidence,
            max_components,
            include_edges,
            budget,
        )
    }

    /// Builds a deterministic bounded architecture plan with typed scope and detail.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid scope, budget, confidence, component,
    /// view, arithmetic, or admission bounds.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded architecture-overview dimension"
    )]
    pub fn plan_architecture_overview_with_options(
        &self,
        mut views: Vec<ArchitectureOverviewView>,
        scope: Option<AnalysisScope>,
        detail: ArchitectureOverviewDetail,
        min_confidence: u16,
        max_components: usize,
        include_edges: bool,
        budget: QueryBudget,
    ) -> Result<ArchitectureOverviewPlan, QueryError> {
        budget.validate()?;
        validate_analysis_scope(scope.as_ref())?;
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
        let mut operators = vec![
            QueryOperator::GenerationPin,
            QueryOperator::RelationScan,
            QueryOperator::EntityLookup,
            QueryOperator::AggregateGraph,
        ];
        if views.contains(&ArchitectureOverviewView::Communities) {
            operators.push(QueryOperator::CommunityView);
        }
        if views.contains(&ArchitectureOverviewView::Hotspots) {
            operators.push(QueryOperator::HotspotRank);
        }
        operators.push(QueryOperator::OutputBudget);
        let explanation = PlanExplanation {
            generation: self.generation.metadata().generation(),
            kind: PlanKind::ArchitectureOverview,
            operators,
            estimate,
        };
        Ok(ArchitectureOverviewPlan {
            views,
            scope,
            detail,
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
            communities: overview.communities,
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
        test_kinds: Vec<TestsSelectKind>,
        max_tests: usize,
        include_commands: bool,
        budget: QueryBudget,
    ) -> Result<TestsSelectPlan, QueryError> {
        self.plan_tests_select_with_filters(
            seeds,
            Vec::new(),
            Vec::new(),
            test_kinds,
            Vec::new(),
            max_tests,
            None,
            None,
            include_commands,
            budget,
        )
    }

    /// Builds a deterministic bounded `tests.select` plan with every public filter.
    ///
    /// Path and build-target seeds are resolved only inside the pinned
    /// generation. Framework and execution-budget filters affect deterministic
    /// candidate admission but never execute a command.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid bounds, an empty or oversized
    /// aggregate seed selector, invalid filter cardinality, arithmetic
    /// overflow, or a conservative estimate that cannot be admitted.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded public test-selection dimension"
    )]
    pub fn plan_tests_select_with_filters(
        &self,
        seeds: BTreeSet<SymbolId>,
        mut seed_paths: Vec<String>,
        mut seed_build_targets: Vec<String>,
        mut test_kinds: Vec<TestsSelectKind>,
        mut frameworks: Vec<String>,
        max_tests: usize,
        max_total_ms: Option<u32>,
        max_slow_tests: Option<u16>,
        include_commands: bool,
        budget: QueryBudget,
    ) -> Result<TestsSelectPlan, QueryError> {
        budget.validate()?;
        let aggregate_seed_count = seeds
            .len()
            .checked_add(seed_paths.len())
            .and_then(|count| count.checked_add(seed_build_targets.len()))
            .ok_or(QueryError::PlanRejected {
                resource: QueryResource::Results,
            })?;
        if aggregate_seed_count == 0
            || seeds.len() > 64
            || seed_paths.len() > 256
            || seed_build_targets.len() > 128
        {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if test_kinds.len() > 6 || frameworks.len() > 32 {
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
        seed_paths.sort();
        seed_paths.dedup();
        seed_build_targets.sort();
        seed_build_targets.dedup();
        test_kinds.sort();
        test_kinds.dedup();
        frameworks.sort();
        frameworks.dedup();
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
            seed_paths,
            seed_build_targets,
            test_kinds,
            frameworks,
            max_tests,
            max_total_ms,
            max_slow_tests,
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
        changed_paths: Vec<String>,
        max_depth: u8,
        min_confidence: u16,
        include_tests: bool,
        max_dependents: usize,
        budget: QueryBudget,
    ) -> Result<ChangeImpactPlan, QueryError> {
        self.plan_change_impact_with_policy(
            changed_symbols,
            changed_paths,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ChangeImpactRelationPolicy::Standard,
            max_depth,
            min_confidence,
            include_tests,
            false,
            max_dependents,
            budget,
        )
    }

    /// Builds a deterministic bounded `change.impact` plan with scope and policy.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid budget, empty or oversized change
    /// set, invalid scope, out-of-range depth, confidence, or dependent bounds,
    /// arithmetic overflow, or an estimate that cannot be admitted.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded public impact-analysis dimension"
    )]
    pub fn plan_change_impact_with_policy(
        &self,
        changed_symbols: BTreeSet<SymbolId>,
        mut changed_paths: Vec<String>,
        mut scope_paths: Vec<String>,
        mut scope_packages: Vec<String>,
        mut scope_services: Vec<String>,
        relation_policy: ChangeImpactRelationPolicy,
        max_depth: u8,
        min_confidence: u16,
        include_tests: bool,
        include_history: bool,
        max_dependents: usize,
        budget: QueryBudget,
    ) -> Result<ChangeImpactPlan, QueryError> {
        budget.validate()?;
        let max_depth = if relation_policy == ChangeImpactRelationPolicy::DirectOnly {
            1
        } else {
            max_depth
        };
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
        if scope_paths.len() > 256 || scope_packages.len() > 128 || scope_services.len() > 64 {
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
        scope_paths.sort();
        scope_paths.dedup();
        scope_packages.sort();
        scope_packages.dedup();
        scope_services.sort();
        scope_services.dedup();
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
            scope_paths,
            scope_packages,
            scope_services,
            relation_policy,
            max_depth,
            min_confidence,
            include_tests,
            include_history,
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
    /// depth and dependent bounds. The context-aware entry point additionally
    /// resolves repository-relative paths and carries caller constraints into
    /// an explicit final verification step.
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
        self.plan_plan_change_with_context(
            objective,
            objective_text,
            target_symbols,
            target_files,
            BTreeSet::new(),
            Vec::new(),
            max_steps,
            budget,
        )
    }

    /// Builds a deterministic change plan with admitted path context and constraints.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] under the same admission and budget conditions as
    /// [`Self::plan_plan_change`].
    #[expect(
        clippy::too_many_arguments,
        reason = "change context and caller constraints are independent bounded plan dimensions"
    )]
    pub fn plan_plan_change_with_context(
        &self,
        objective: PlanChangeObjective,
        objective_text: String,
        target_symbols: BTreeSet<SymbolId>,
        target_files: BTreeSet<FileId>,
        target_paths: BTreeSet<String>,
        constraints: Vec<String>,
        max_steps: usize,
        budget: QueryBudget,
    ) -> Result<PlanChangePlan, QueryError> {
        budget.validate()?;
        if target_symbols.is_empty() && target_files.is_empty() && target_paths.is_empty() {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if target_symbols.len() > 64
            || target_files.len() > 64
            || target_paths.len() > 1_000
            || constraints.len() > 32
            || constraints
                .iter()
                .any(|constraint| constraint.is_empty() || constraint.chars().count() > 1_024)
        {
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
            target_paths,
            constraints,
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
    /// result cap are validated here. Call
    /// [`Self::plan_history_compare_with_scope`] when the request includes
    /// structural scope or unchanged-lineage context.
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
        self.plan_history_compare_with_scope(
            base_generation,
            HistoryCompareScope::default(),
            change_kinds,
            false,
            max_results,
            budget,
        )
    }

    /// Builds a bounded comparison plan with combined structural scope.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid scope, budget, or result bounds.
    pub fn plan_history_compare_with_scope(
        &self,
        base_generation: GenerationId,
        scope: HistoryCompareScope,
        change_kinds: BTreeSet<HistoryChangeKind>,
        include_unchanged_context: bool,
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
        if scope.paths.len() > 256
            || scope.packages.len() > 128
            || scope.services.len() > 64
            || scope.symbols.len() > 256
            || scope.paths.iter().any(|path| path.is_empty())
            || scope.packages.iter().any(|package| package.is_empty())
            || scope.services.iter().any(|service| service.is_empty())
        {
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
            scope,
            change_kinds,
            include_unchanged_context,
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
    /// reports scoped component-boundary and cross-service dependency deltas.
    /// Rows, edges, results, and memory are measured exactly like
    /// `change.impact`.
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
        let (operators, depth) = ast.derive_plan_shape();
        validate_advanced_depths(&ast, depth, max_depth)?;
        let estimated_cost =
            AdvancedQueryPlan::validate(&operators, max_results, max_traversal, depth)?;
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
        // The query ceiling includes chunk metadata, so only the remaining
        // memory can be delegated to source response materialization.
        let response_memory = budget
            .max_memory_bytes
            .checked_sub(chunk_memory)
            .filter(|remaining| *remaining > 0)
            .ok_or(QueryError::PlanRejected {
                resource: QueryResource::MemoryBytes,
            })?
            .min(checked_usize_to_u64(
                source_budget.max_response_memory_bytes,
            )?);
        source_budget.max_response_memory_bytes =
            usize::try_from(response_memory).map_err(|_| QueryError::MemoryUnavailable)?;
        let memory_bytes = checked_add(
            response_memory,
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
    let mut file_paths: BTreeMap<FileId, &str> = BTreeMap::new();
    for file in &document.files {
        tracker.add_memory(advanced_file_path_index_entry_bytes()?)?;
        file_paths.insert(file.id, file.path.as_str());
    }

    let materialization_cap = plan
        .page_offset
        .checked_add(plan.max_rows)
        .and_then(|cap| cap.checked_add(1))
        .ok_or(QueryError::MemoryUnavailable)?;
    let (mut set, truncated) = eval_advanced_node(
        document,
        &plan.ast,
        &file_paths,
        Some(materialization_cap),
        control,
        tracker,
    )?;

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

fn validate_advanced_depths(
    ast: &AdvancedAstNode,
    plan_depth: usize,
    max_depth: usize,
) -> Result<(), QueryError> {
    if max_depth == 0
        || max_depth > ADVANCED_MAX_DEPTH
        || plan_depth > max_depth
        || !advanced_traversal_depths_within(ast, max_depth)
    {
        return Err(QueryError::PlanRejected {
            resource: QueryResource::Depth,
        });
    }
    Ok(())
}

fn advanced_traversal_depths_within(node: &AdvancedAstNode, max_depth: usize) -> bool {
    match node {
        AdvancedAstNode::Scan { .. } => true,
        AdvancedAstNode::Filter { input, .. }
        | AdvancedAstNode::Project { input, .. }
        | AdvancedAstNode::Aggregate { input, .. }
        | AdvancedAstNode::Sort { input, .. }
        | AdvancedAstNode::Limit { input, .. } => {
            advanced_traversal_depths_within(input, max_depth)
        }
        AdvancedAstNode::Join { left, right, .. } => {
            advanced_traversal_depths_within(left, max_depth)
                && advanced_traversal_depths_within(right, max_depth)
        }
        AdvancedAstNode::Traverse {
            max_depth: traversal_depth,
            ..
        } => traversal_depth.is_none_or(|depth| depth > 0 && usize::from(depth) <= max_depth),
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
    file_paths: &BTreeMap<FileId, &str>,
    output_cap: Option<usize>,
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
                output_cap,
                control,
                tracker,
            )?;
            Ok((set, false))
        }
        AdvancedAstNode::Filter { input, predicate } => {
            let (set, truncated) =
                eval_advanced_node(document, input, file_paths, None, control, tracker)?;
            let mut set = advanced_filter_rows(set, predicate, control)?;
            cap_advanced_rows(&mut set, output_cap);
            Ok((set, truncated))
        }
        AdvancedAstNode::Project { input, columns } => {
            let (set, truncated) =
                eval_advanced_node(document, input, file_paths, output_cap, control, tracker)?;
            Ok((
                advanced_project_rows(set, columns, control, tracker)?,
                truncated,
            ))
        }
        AdvancedAstNode::Sort { input, by } => {
            let (mut set, truncated) =
                eval_advanced_node(document, input, file_paths, None, control, tracker)?;
            advanced_sort_rows(&mut set.rows, by, control, tracker)?;
            cap_advanced_rows(&mut set, output_cap);
            Ok((set, truncated))
        }
        AdvancedAstNode::Limit { input, max_rows } => {
            let limit = usize::from(*max_rows);
            let input_cap = Some(output_cap.map_or(limit, |cap| cap.min(limit)));
            let (set, truncated) =
                eval_advanced_node(document, input, file_paths, input_cap, control, tracker)?;
            Ok((advanced_limit_rows(set, limit, control)?, truncated))
        }
        AdvancedAstNode::Join { left, right, on } => {
            let (left, left_truncated) =
                eval_advanced_node(document, left, file_paths, None, control, tracker)?;
            let (right, right_truncated) =
                eval_advanced_node(document, right, file_paths, None, control, tracker)?;
            let mut joined = advanced_join_rows(left, right, on, control, tracker)?;
            cap_advanced_rows(&mut joined, output_cap);
            Ok((joined, left_truncated || right_truncated))
        }
        AdvancedAstNode::Aggregate {
            input,
            group_by,
            aggregations,
        } => {
            let (input, truncated) =
                eval_advanced_node(document, input, file_paths, None, control, tracker)?;
            let mut aggregated =
                advanced_aggregate_rows(input, group_by, aggregations, control, tracker)?;
            cap_advanced_rows(&mut aggregated, output_cap);
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
            let mut traversed = advanced_traverse_rows(
                document,
                seed,
                *relation,
                *direction,
                max_depth.unwrap_or(1),
                control,
                tracker,
            )?;
            cap_advanced_rows(&mut traversed, output_cap);
            Ok((traversed, false))
        }
    }
}

/// Scans entities of one kind, optionally filtering by a bounded predicate.
fn eval_advanced_scan(
    document: &NormalizedIrDocument,
    entity_kind: AdvancedEntityKind,
    filter: Option<&AdvancedPredicate>,
    file_paths: &BTreeMap<FileId, &str>,
    output_cap: Option<usize>,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
) -> Result<AdvancedRowSet, QueryError> {
    let columns = advanced_default_scan_columns();
    let mut matched_count = 0_usize;
    for entity in &document.entities {
        control.check()?;
        if entity_kind.matches_ir(entity.kind) {
            tracker.add_rows(1)?;
            matched_count = matched_count
                .checked_add(1)
                .ok_or(QueryError::MemoryUnavailable)?;
        }
    }

    tracker.add_memory(advanced_vector_bytes::<&rootlight_ir::EntityRecord>(
        matched_count,
    )?)?;
    let mut matched = Vec::new();
    matched
        .try_reserve_exact(matched_count)
        .map_err(|_| QueryError::MemoryUnavailable)?;
    for entity in &document.entities {
        control.check()?;
        if entity_kind.matches_ir(entity.kind) {
            matched.push(entity);
        }
    }
    // Deterministic identity order independent of document insertion order.
    matched.sort_by_key(|entity| entity.id);

    let output_cap = output_cap.unwrap_or(usize::MAX);
    let row_capacity = matched_count.min(output_cap);
    tracker.add_memory(advanced_vector_bytes::<BTreeMap<String, AdvancedValue>>(
        row_capacity,
    )?)?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(row_capacity)
        .map_err(|_| QueryError::MemoryUnavailable)?;
    for entity in matched {
        if rows.len() >= output_cap {
            break;
        }
        control.check()?;
        let kind = serialized_label(&entity.kind)?;
        let path = entity
            .evidence
            .source
            .as_ref()
            .and_then(|source| file_paths.get(&source.span().file()).copied())
            .unwrap_or_default();
        tracker.add_memory(advanced_scan_row_owned_bytes(entity, &kind, path)?)?;
        let row = advanced_scan_row(entity, kind, path);
        if filter.is_some_and(|predicate| !advanced_predicate_matches(predicate, &row)) {
            continue;
        }
        rows.push(row);
    }
    Ok(AdvancedRowSet { columns, rows })
}

/// Builds one scan row keyed by the default scan columns.
fn advanced_scan_row(
    entity: &rootlight_ir::EntityRecord,
    kind: String,
    path: &str,
) -> BTreeMap<String, AdvancedValue> {
    let mut row = BTreeMap::new();
    row.insert("id".to_owned(), AdvancedValue::Symbol(entity.id));
    row.insert("kind".to_owned(), AdvancedValue::Text(kind));
    row.insert(
        "name".to_owned(),
        AdvancedValue::Text(entity.canonical_name.clone()),
    );
    row.insert("path".to_owned(), AdvancedValue::Text(path.to_owned()));
    row
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
    tracker: &mut UsageTracker,
) -> Result<AdvancedRowSet, QueryError> {
    let schema = advanced_project_schema(&set.columns, columns);
    let mut rows = Vec::new();
    tracker.add_memory(advanced_vector_bytes::<BTreeMap<String, AdvancedValue>>(
        set.rows.len(),
    )?)?;
    rows.try_reserve_exact(set.rows.len())
        .map_err(|_| QueryError::MemoryUnavailable)?;
    for row in set.rows {
        control.check()?;
        tracker.add_memory(advanced_projected_row_owned_bytes(&row, columns)?)?;
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

fn cap_advanced_rows(set: &mut AdvancedRowSet, cap: Option<usize>) {
    if let Some(cap) = cap {
        set.rows.truncate(cap);
    }
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

fn advanced_file_path_index_entry_bytes() -> Result<u64, QueryError> {
    checked_usize_to_u64(
        ADVANCED_BTREE_ENTRY_OVERHEAD_BYTES
            .saturating_add(mem::size_of::<FileId>())
            .saturating_add(mem::size_of::<&str>()),
    )
}

fn advanced_vector_bytes<T>(len: usize) -> Result<u64, QueryError> {
    let bytes = len
        .checked_mul(mem::size_of::<T>())
        .ok_or(QueryError::MemoryUnavailable)?;
    checked_usize_to_u64(bytes)
}

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

fn advanced_scan_row_owned_bytes(
    entity: &rootlight_ir::EntityRecord,
    kind: &str,
    path: &str,
) -> Result<u64, QueryError> {
    let mut bytes = mem::size_of::<BTreeMap<String, AdvancedValue>>();
    for (name, dynamic_bytes) in [
        ("id", 0),
        ("kind", kind.len()),
        ("name", entity.canonical_name.len()),
        ("path", path.len()),
    ] {
        bytes = bytes
            .saturating_add(ADVANCED_BTREE_ENTRY_OVERHEAD_BYTES)
            .saturating_add(mem::size_of::<String>())
            .saturating_add(name.len())
            .saturating_add(mem::size_of::<AdvancedValue>())
            .saturating_add(dynamic_bytes);
    }
    checked_usize_to_u64(bytes)
}

fn advanced_projected_row_owned_bytes(
    row: &BTreeMap<String, AdvancedValue>,
    columns: &[String],
) -> Result<u64, QueryError> {
    let mut bytes = mem::size_of::<BTreeMap<String, AdvancedValue>>();
    for name in columns {
        let Some(value) = row.get(name) else {
            continue;
        };
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

fn validate_analysis_scope(scope: Option<&AnalysisScope>) -> Result<(), QueryError> {
    let invalid_label = |value: &str, maximum: usize| {
        value.is_empty() || value.len() > maximum || value.as_bytes().contains(&0)
    };
    let valid = match scope {
        None => true,
        Some(AnalysisScope::Paths(paths)) => {
            !paths.is_empty()
                && paths.len() <= 256
                && paths.iter().all(|path| {
                    !invalid_label(path, 8_192)
                        && !path.starts_with('/')
                        && !path.starts_with('\\')
                        && !matches!(path.get(1..3), Some(":\\") | Some(":/"))
                        && !path.split(['/', '\\']).any(|component| component == "..")
                })
        }
        Some(AnalysisScope::Packages(packages)) => {
            !packages.is_empty()
                && packages.len() <= 256
                && packages.iter().all(|package| !invalid_label(package, 512))
        }
        Some(AnalysisScope::BuildTargets(targets)) => {
            !targets.is_empty()
                && targets.len() <= 256
                && targets.iter().all(|target| !invalid_label(target, 512))
        }
        Some(AnalysisScope::Symbols(symbols)) => !symbols.is_empty() && symbols.len() <= 64,
    };
    if valid {
        Ok(())
    } else {
        Err(QueryError::PlanRejected {
            resource: QueryResource::Results,
        })
    }
}

fn path_matches_scope(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        let normalized = prefix.trim_end_matches(['/', '\\']);
        path == normalized
            || path
                .strip_prefix(normalized)
                .is_some_and(|suffix| suffix.starts_with('/') || suffix.starts_with('\\'))
    })
}

fn entity_matches_label(entity: &rootlight_ir::EntityRecord, labels: &[String]) -> bool {
    labels.iter().any(|label| {
        label == &entity.id.to_string()
            || label == &entity.canonical_name
            || label == &entity.display_name
            || label == &entity.qualified_name
    })
}

fn entity_parent_map(document: &NormalizedIrDocument) -> BTreeMap<SymbolId, SymbolId> {
    let mut parents = BTreeMap::new();
    for entity in &document.entities {
        if let Some(ContainerRef::Entity(parent)) = entity.container {
            parents.insert(entity.id, parent);
        }
    }
    for relation in &document.relations {
        if relation.predicate == RelationPredicate::Contains
            && let (RelationEndpoint::Entity(parent), RelationEndpoint::Entity(child)) =
                (relation.subject, relation.object)
        {
            parents.entry(child).or_insert(parent);
        }
    }
    parents
}

fn entity_descends_from(
    symbol: SymbolId,
    roots: &BTreeSet<SymbolId>,
    parents: &BTreeMap<SymbolId, SymbolId>,
) -> bool {
    let mut cursor = symbol;
    let mut visited = BTreeSet::new();
    loop {
        if roots.contains(&cursor) {
            return true;
        }
        if !visited.insert(cursor) {
            return false;
        }
        let Some(parent) = parents.get(&cursor).copied() else {
            return false;
        };
        cursor = parent;
    }
}

fn analysis_scope_entities(
    document: &NormalizedIrDocument,
    scope: Option<&AnalysisScope>,
) -> BTreeSet<SymbolId> {
    let Some(scope) = scope else {
        return document.entities.iter().map(|entity| entity.id).collect();
    };
    match scope {
        AnalysisScope::Paths(prefixes) => document
            .entities
            .iter()
            .filter(|entity| {
                entity
                    .evidence
                    .source
                    .as_ref()
                    .and_then(|source| find_file(document, source.span().file()))
                    .is_some_and(|file| path_matches_scope(&file.path, prefixes))
            })
            .map(|entity| entity.id)
            .collect(),
        AnalysisScope::Symbols(symbols) => {
            let parents = entity_parent_map(document);
            document
                .entities
                .iter()
                .filter(|entity| entity_descends_from(entity.id, symbols, &parents))
                .map(|entity| entity.id)
                .collect()
        }
        AnalysisScope::Packages(labels) | AnalysisScope::BuildTargets(labels) => {
            let expected_kind = if matches!(scope, AnalysisScope::Packages(_)) {
                EntityKind::Package
            } else {
                EntityKind::BuildTarget
            };
            let roots: BTreeSet<SymbolId> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == expected_kind && entity_matches_label(entity, labels)
                })
                .map(|entity| entity.id)
                .collect();
            let parents = entity_parent_map(document);
            document
                .entities
                .iter()
                .filter(|entity| entity_descends_from(entity.id, &roots, &parents))
                .map(|entity| entity.id)
                .collect()
        }
    }
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

/// Maximum confidence of a syntax-fallback dispatch candidate.
///
/// Tier-D analysis can identify a bounded lexical candidate, but it cannot
/// establish a semantic call. Keeping the ceiling in the weak-evidence band
/// prevents downstream query projections from promoting that candidate merely
/// because a producer supplied an optimistic raw confidence.
const TIER_D_DISPATCH_MAX_CONFIDENCE: u16 = 399;

/// Returns relation confidence after applying query-level semantic ceilings.
fn effective_relation_confidence(
    document: &NormalizedIrDocument,
    relation: &rootlight_ir::RelationRecord,
) -> u16 {
    let confidence = relation.confidence.get();
    if relation.predicate != RelationPredicate::DispatchCandidate {
        return confidence;
    }
    let tier_d_provenance = document
        .provenance
        .binary_search_by_key(&relation.provenance, |record| record.id)
        .ok()
        .and_then(|index| document.provenance.get(index))
        .is_some_and(|record| matches!(record.tier, AnalysisTier::TierD));
    let tier_d_endpoint = [relation.subject, relation.object]
        .into_iter()
        .filter_map(|endpoint| endpoint_entity(document, endpoint))
        .filter_map(|symbol| find_entity(document, symbol))
        .any(|entity| matches!(entity.tier, AnalysisTier::TierD));
    if tier_d_provenance || tier_d_endpoint {
        confidence.min(TIER_D_DISPATCH_MAX_CONFIDENCE)
    } else {
        confidence
    }
}

/// Repository-wide completeness needed to qualify negative query conclusions.
#[derive(Debug, Default)]
struct RepositoryCoverageSummary {
    entities_complete: bool,
    relations_structural_complete: bool,
    relations_semantic_complete: bool,
    truncated: bool,
}

/// Returns whether one repository coverage record is internally complete.
fn coverage_record_is_complete(record: &CoverageRecord) -> bool {
    matches!(record.status, CoverageStatus::Complete)
        && record.indexed == record.discovered
        && record.skipped == 0
}

/// Reads repository-level entity and relation coverage under the query budget.
///
/// File- or entity-scoped records cannot establish an exhaustive repository
/// negative. Semantic relation completeness additionally requires Tier A or B;
/// Tier C can establish structural imports, while Tier D is syntax fallback.
fn repository_coverage_summary(
    document: &NormalizedIrDocument,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<RepositoryCoverageSummary, QueryError> {
    let mut entity_seen = false;
    let mut entities_complete = true;
    let mut relation_seen = false;
    let mut relations_structural_complete = true;
    let mut relations_semantic_complete = true;
    let mut summary = RepositoryCoverageSummary::default();

    for record in &document.coverage_records {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            summary.truncated = true;
            break;
        }
        tracker.add_rows(1)?;
        if record.scope != CoverageScope::Repository(document.repository) {
            continue;
        }
        match record.domain {
            FactDomain::Entities => {
                entity_seen = true;
                entities_complete &= coverage_record_is_complete(record);
            }
            FactDomain::Relations => {
                relation_seen = true;
                let complete = coverage_record_is_complete(record);
                relations_structural_complete &=
                    complete && !matches!(record.tier, AnalysisTier::TierD);
                relations_semantic_complete &=
                    complete && matches!(record.tier, AnalysisTier::TierA | AnalysisTier::TierB);
            }
            _ => {}
        }
    }

    if !summary.truncated {
        summary.entities_complete = entity_seen && entities_complete;
        summary.relations_structural_complete = relation_seen && relations_structural_complete;
        summary.relations_semantic_complete = relation_seen && relations_semantic_complete;
    }
    Ok(summary)
}

/// Returns whether coverage can make one relationship-family result exhaustive.
fn relationship_families_are_complete(
    families: &[RelationFamily],
    coverage: &RepositoryCoverageSummary,
) -> bool {
    families.iter().all(|family| match family {
        RelationFamily::Imports => coverage.relations_structural_complete,
        RelationFamily::Calls
        | RelationFamily::CalledBy
        | RelationFamily::References
        | RelationFamily::Types
        | RelationFamily::Implements => coverage.relations_semantic_complete,
        RelationFamily::Tests
        | RelationFamily::Ownership
        | RelationFamily::ServiceCall
        | RelationFamily::CallsRoute
        | RelationFamily::Messaging
        | RelationFamily::ReadsTable
        | RelationFamily::WritesTable
        | RelationFamily::BuildDependency
        | RelationFamily::DataFlow
        | RelationFamily::History => false,
    })
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
        let confidence = effective_relation_confidence(document, relation);
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

// Cycle execution retains the scoped projection, adjacency, and Tarjan state
// together. These charges conservatively cover B-tree nodes, vector capacity,
// copied source evidence, component membership, and traversal queues.
const CYCLE_ADJACENCY_FIXED_WORKSPACE_BYTES: usize = 64 * 1024;
const CYCLE_ADJACENCY_ENTITY_WORKSPACE_BYTES: usize = 384;
const CYCLE_ADJACENCY_RELATION_WORKSPACE_BYTES: usize = 256;
const CYCLE_DETECTION_FIXED_WORKSPACE_BYTES: usize = 32 * 1024;
const CYCLE_DETECTION_NODE_WORKSPACE_BYTES: usize = 640;

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
    RelationFamily::Tests,
    RelationFamily::Ownership,
    RelationFamily::ServiceCall,
    RelationFamily::Messaging,
    RelationFamily::ReadsTable,
    RelationFamily::WritesTable,
    RelationFamily::BuildDependency,
    RelationFamily::DataFlow,
    RelationFamily::History,
];

/// Aggregated architecture overview assembled before bounded result emission.
struct ArchitectureOverviewAnalysis {
    components: Vec<ArchitectureComponent>,
    connections: Vec<ArchitectureConnection>,
    hotspots: Vec<ArchitectureHotspot>,
    communities: Vec<ArchitectureCommunity>,
    views: Vec<ArchitectureOverviewDerivedView>,
}

const ARCHITECTURE_COMMUNITY_SEED: u64 = 0x524f_4f54_4c49_4748;
const ARCHITECTURE_COMMUNITY_MAX_ITERATIONS: usize = 8;

// The overview keeps several B-tree indexes and owned component labels alive
// through result emission. These conservative charges cover tree nodes, string
// capacity, vector slack, graph aggregation, and community projection.
const ARCHITECTURE_OVERVIEW_FIXED_WORKSPACE_BYTES: usize = 64 * 1024;
const ARCHITECTURE_OVERVIEW_FILE_WORKSPACE_BYTES: usize = 256;
const ARCHITECTURE_OVERVIEW_ENTITY_WORKSPACE_BYTES: usize = 1_280;
const ARCHITECTURE_OVERVIEW_RELATION_WORKSPACE_BYTES: usize = 384;
const ARCHITECTURE_OVERVIEW_COMMUNITY_RELATION_BYTES: usize = 256;

/// Returns the stable algorithm-version label for one derived view.
const fn architecture_overview_algorithm_version(view: ArchitectureOverviewView) -> &'static str {
    match view {
        ArchitectureOverviewView::Modules
        | ArchitectureOverviewView::Packages
        | ArchitectureOverviewView::Services
        | ArchitectureOverviewView::Data
        | ArchitectureOverviewView::Build => "typed_entity_aggregation_v1",
        ArchitectureOverviewView::Ownership => "normalized_ownership_projection_v1",
        ArchitectureOverviewView::Communities => "weighted_label_propagation_v1",
        ArchitectureOverviewView::Hotspots => "fan_in_out_v1",
    }
}

fn architecture_overview_parameters(view: ArchitectureOverviewView) -> BTreeMap<String, String> {
    match view {
        ArchitectureOverviewView::Modules => {
            BTreeMap::from([("entity_kinds".to_owned(), "module,namespace".to_owned())])
        }
        ArchitectureOverviewView::Packages => {
            BTreeMap::from([("entity_kinds".to_owned(), "package".to_owned())])
        }
        ArchitectureOverviewView::Services => {
            BTreeMap::from([("entity_kinds".to_owned(), "service,route".to_owned())])
        }
        ArchitectureOverviewView::Data => BTreeMap::from([(
            "entity_kinds".to_owned(),
            "database_object,message_topic".to_owned(),
        )]),
        ArchitectureOverviewView::Build => {
            BTreeMap::from([("entity_kinds".to_owned(), "build_target".to_owned())])
        }
        ArchitectureOverviewView::Ownership => BTreeMap::from([
            (
                "relation_family".to_owned(),
                RelationFamily::Ownership.as_str().to_owned(),
            ),
            (
                "coverage".to_owned(),
                if RelationFamily::Ownership.predicates().is_empty() {
                    "unavailable"
                } else {
                    "served"
                }
                .to_owned(),
            ),
        ]),
        ArchitectureOverviewView::Communities => BTreeMap::from([
            (
                "graph".to_owned(),
                "undirected_weighted_component_relations".to_owned(),
            ),
            (
                "max_iterations".to_owned(),
                ARCHITECTURE_COMMUNITY_MAX_ITERATIONS.to_string(),
            ),
            ("ownership_truth".to_owned(), "not_claimed".to_owned()),
            (
                "seed".to_owned(),
                format!("{ARCHITECTURE_COMMUNITY_SEED:016x}"),
            ),
        ]),
        ArchitectureOverviewView::Hotspots => BTreeMap::from([
            (
                "normalization".to_owned(),
                "max_fan_in_plus_fan_out".to_owned(),
            ),
            ("score_range".to_owned(), "0..1000".to_owned()),
        ]),
    }
}

fn seeded_community_rank(component: &str) -> u64 {
    let digest = content_hash(component.as_bytes());
    let mut prefix = [0_u8; size_of::<u64>()];
    prefix.copy_from_slice(&digest.as_bytes()[..size_of::<u64>()]);
    u64::from_be_bytes(prefix) ^ ARCHITECTURE_COMMUNITY_SEED
}

fn architecture_community_id(members: &[String]) -> Result<String, QueryError> {
    const IDENTITY_CONTEXT: &[u8] =
        b"rootlight/architecture-community/weighted-label-propagation/v1";

    let member_bytes = members.iter().try_fold(
        IDENTITY_CONTEXT.len() + size_of::<u64>(),
        |bytes, member| {
            bytes
                .checked_add(member.len())
                .and_then(|bytes| bytes.checked_add(1))
                .ok_or(QueryError::MemoryUnavailable)
        },
    )?;
    let mut identity = Vec::new();
    identity
        .try_reserve_exact(member_bytes)
        .map_err(|_| QueryError::MemoryUnavailable)?;
    identity.extend_from_slice(IDENTITY_CONTEXT);
    identity.extend_from_slice(&ARCHITECTURE_COMMUNITY_SEED.to_be_bytes());
    for member in members {
        identity.extend_from_slice(member.as_bytes());
        identity.push(0);
    }
    Ok(format!("community:{}", content_hash(&identity)))
}

fn build_architecture_communities(
    component_ids: &[String],
    aggregated: &BTreeMap<(String, String, RelationFamily), (u32, u16)>,
    control: &QueryControl<'_>,
) -> Result<Vec<ArchitectureCommunity>, QueryError> {
    let mut adjacency: BTreeMap<String, BTreeMap<String, u64>> = component_ids
        .iter()
        .cloned()
        .map(|component| (component, BTreeMap::new()))
        .collect();
    for ((from, to, _family), (weight, _confidence)) in aggregated {
        control.check()?;
        if let Some(neighbors) = adjacency.get_mut(from) {
            let entry = neighbors.entry(to.clone()).or_insert(0);
            *entry = entry.saturating_add(u64::from(*weight));
        }
        if let Some(neighbors) = adjacency.get_mut(to) {
            let entry = neighbors.entry(from.clone()).or_insert(0);
            *entry = entry.saturating_add(u64::from(*weight));
        }
    }

    let mut labels: BTreeMap<String, String> = component_ids
        .iter()
        .cloned()
        .map(|component| (component.clone(), component))
        .collect();
    for _ in 0..ARCHITECTURE_COMMUNITY_MAX_ITERATIONS {
        control.check()?;
        let mut next = labels.clone();
        for component in component_ids {
            control.check()?;
            let current = labels
                .get(component)
                .cloned()
                .unwrap_or_else(|| component.clone());
            let mut scores = BTreeMap::from([(current.clone(), 1_u64)]);
            if let Some(neighbors) = adjacency.get(component) {
                for (neighbor, weight) in neighbors {
                    // Canonical file order makes this in-place propagation
                    // deterministic while preventing two-node label swapping.
                    let label = next
                        .get(neighbor)
                        .cloned()
                        .unwrap_or_else(|| neighbor.clone());
                    let score = scores.entry(label).or_insert(0);
                    *score = score.saturating_add(*weight);
                }
            }
            let selected = scores
                .into_iter()
                .max_by(|(left_label, left_score), (right_label, right_score)| {
                    left_score.cmp(right_score).then_with(|| {
                        seeded_community_rank(right_label)
                            .cmp(&seeded_community_rank(left_label))
                            .then_with(|| right_label.cmp(left_label))
                    })
                })
                .map_or(current, |(label, _score)| label);
            next.insert(component.clone(), selected);
        }
        if next == labels {
            break;
        }
        labels = next;
    }

    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for component in component_ids {
        grouped
            .entry(
                labels
                    .get(component)
                    .cloned()
                    .unwrap_or_else(|| component.clone()),
            )
            .or_default()
            .push(component.clone());
    }
    let mut communities = Vec::new();
    for (_label, mut members) in grouped {
        control.check()?;
        members.sort();
        let member_set: BTreeSet<String> = members.iter().cloned().collect();
        let internal_connection_weight = aggregated
            .iter()
            .filter(|((from, to, _family), _)| member_set.contains(from) && member_set.contains(to))
            .fold(0_u64, |total, (_edge, (weight, _confidence))| {
                total.saturating_add(u64::from(*weight))
            });
        communities.push(ArchitectureCommunity {
            id: architecture_community_id(&members)?,
            members,
            internal_connection_weight,
            ownership_truth: false,
        });
    }
    communities.sort_by(|left, right| {
        right
            .members
            .len()
            .cmp(&left.members.len())
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(communities)
}

/// Returns the repository-controlled display path for one file, falling back to
/// the stable file identity when the file record is not served.
fn architecture_file_name(document: &NormalizedIrDocument, file: FileId) -> String {
    find_file(document, file)
        .map(|record| record.path.clone())
        .unwrap_or_else(|| file.to_string())
}

fn architecture_view_kinds(view: ArchitectureOverviewView) -> &'static [EntityKind] {
    match view {
        ArchitectureOverviewView::Modules => &[EntityKind::Module, EntityKind::Namespace],
        ArchitectureOverviewView::Packages => &[EntityKind::Package],
        ArchitectureOverviewView::Services => &[EntityKind::Service, EntityKind::Route],
        ArchitectureOverviewView::Data => &[EntityKind::DatabaseObject, EntityKind::MessageTopic],
        ArchitectureOverviewView::Build => &[EntityKind::BuildTarget],
        ArchitectureOverviewView::Ownership
        | ArchitectureOverviewView::Communities
        | ArchitectureOverviewView::Hotspots => &[],
    }
}

fn architecture_component_root(
    document: &NormalizedIrDocument,
    parents: &BTreeMap<SymbolId, SymbolId>,
    symbol: SymbolId,
    views: &[ArchitectureOverviewView],
) -> Option<SymbolId> {
    let mut cursor = symbol;
    let mut visited = BTreeSet::new();
    loop {
        let entity = find_entity(document, cursor)?;
        if views
            .iter()
            .flat_map(|view| architecture_view_kinds(*view))
            .any(|kind| *kind == entity.kind)
        {
            return Some(cursor);
        }
        if !visited.insert(cursor) {
            return None;
        }
        cursor = parents.get(&cursor).copied()?;
    }
}

const fn architecture_tier_confidence(tier: AnalysisTier) -> u16 {
    match tier {
        AnalysisTier::TierA => 1_000,
        AnalysisTier::TierB => 850,
        AnalysisTier::TierC => 600,
        AnalysisTier::TierD => 300,
        _ => 0,
    }
}

fn architecture_detail_limits(detail: ArchitectureOverviewDetail) -> (usize, usize) {
    match detail {
        ArchitectureOverviewDetail::Summary => (2, 0),
        ArchitectureOverviewDetail::Standard => (8, 4),
        ArchitectureOverviewDetail::Detailed => (16, 16),
    }
}

fn architecture_overview_workspace_bytes(
    document: &NormalizedIrDocument,
    plan: &ArchitectureOverviewPlan,
) -> Result<u64, QueryError> {
    let graph_requested = plan.include_edges
        || plan.views.iter().any(|view| {
            matches!(
                view,
                ArchitectureOverviewView::Communities
                    | ArchitectureOverviewView::Hotspots
                    | ArchitectureOverviewView::Ownership
            )
        });
    let relation_bytes = if graph_requested {
        ARCHITECTURE_OVERVIEW_RELATION_WORKSPACE_BYTES.saturating_add(
            if plan.views.contains(&ArchitectureOverviewView::Communities) {
                ARCHITECTURE_OVERVIEW_COMMUNITY_RELATION_BYTES
            } else {
                0
            },
        )
    } else {
        0
    };
    let mut bytes = ARCHITECTURE_OVERVIEW_FIXED_WORKSPACE_BYTES
        .saturating_add(
            document
                .files
                .len()
                .saturating_mul(ARCHITECTURE_OVERVIEW_FILE_WORKSPACE_BYTES),
        )
        .saturating_add(
            document
                .entities
                .len()
                .saturating_mul(ARCHITECTURE_OVERVIEW_ENTITY_WORKSPACE_BYTES),
        )
        .saturating_add(document.relations.len().saturating_mul(relation_bytes));
    for file in &document.files {
        bytes = bytes.saturating_add(file.path.len().saturating_mul(2));
    }
    for entity in &document.entities {
        bytes = bytes.saturating_add(entity.qualified_name.len().saturating_mul(2));
    }
    checked_usize_to_u64(bytes)
}

/// Builds a bounded typed architecture overview.
///
/// Requested structural views select module, package, service, data, or build
/// roots. When no structural view is requested, file components preserve the
/// compact default. Relations aggregate only between reported components.
fn build_architecture_overview(
    document: &NormalizedIrDocument,
    plan: &ArchitectureOverviewPlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<ArchitectureOverviewAnalysis, QueryError> {
    control.check()?;
    tracker.add_memory(architecture_overview_workspace_bytes(document, plan)?)?;

    let scoped_entities = analysis_scope_entities(document, plan.scope.as_ref());
    let structural_views: Vec<ArchitectureOverviewView> = plan
        .views
        .iter()
        .copied()
        .filter(|view| !architecture_view_kinds(*view).is_empty())
        .collect();
    let file_granularity = structural_views.is_empty();

    let mut entity_evidence_file: BTreeMap<SymbolId, FileId> = BTreeMap::new();
    let mut entity_kind: BTreeMap<SymbolId, String> = BTreeMap::new();
    for entity in &document.entities {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if !scoped_entities.contains(&entity.id) {
            continue;
        }
        if let Some(source) = entity.evidence.source.as_ref() {
            entity_evidence_file.insert(entity.id, source.span().file());
        }
        entity_kind.insert(entity.id, serialized_label(&entity.kind)?);
    }

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
                && scoped_entities.contains(&symbol)
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
        let derived_graph_requested = plan.views.iter().any(|view| {
            matches!(
                view,
                ArchitectureOverviewView::Communities
                    | ArchitectureOverviewView::Hotspots
                    | ArchitectureOverviewView::Ownership
            )
        });
        if (!plan.include_edges && !derived_graph_requested)
            || !allowed.contains(&relation.predicate)
        {
            continue;
        }
        let confidence = effective_relation_confidence(document, relation);
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
        if subject == object
            || !scoped_entities.contains(&subject)
            || !scoped_entities.contains(&object)
        {
            continue;
        }
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            break;
        }
        tracker.add_edges(1)?;
        try_push(&mut raw_edges, (subject, object, family, confidence))?;
    }

    let mut entity_file: BTreeMap<SymbolId, FileId> = entity_evidence_file;
    for (symbol, file) in entity_contains_file {
        entity_file.insert(symbol, file);
    }

    let parents = entity_parent_map(document);
    let mut entity_component: BTreeMap<SymbolId, String> = BTreeMap::new();
    let mut component_members: BTreeMap<String, BTreeSet<SymbolId>> = BTreeMap::new();
    let mut component_kind: BTreeMap<String, String> = BTreeMap::new();
    let mut component_name: BTreeMap<String, String> = BTreeMap::new();
    for symbol in &scoped_entities {
        if file_granularity {
            let Some(file) = entity_file.get(symbol).copied() else {
                continue;
            };
            let id = file.to_string();
            entity_component.insert(*symbol, id.clone());
            component_members
                .entry(id.clone())
                .or_default()
                .insert(*symbol);
            component_kind
                .entry(id.clone())
                .or_insert_with(|| "file".to_owned());
            component_name
                .entry(id)
                .or_insert_with(|| architecture_file_name(document, file));
            continue;
        }
        let Some(root) =
            architecture_component_root(document, &parents, *symbol, &structural_views)
        else {
            continue;
        };
        let Some(root_entity) = find_entity(document, root) else {
            continue;
        };
        let id = root.to_string();
        entity_component.insert(*symbol, id.clone());
        component_members
            .entry(id.clone())
            .or_default()
            .insert(*symbol);
        component_kind
            .entry(id.clone())
            .or_insert(serialized_label(&root_entity.kind)?);
        component_name
            .entry(id)
            .or_insert_with(|| root_entity.qualified_name.clone());
    }

    let mut component_ids: Vec<String> = component_members.keys().cloned().collect();
    component_ids.sort_by(|left, right| {
        let left_count = component_members.get(left).map_or(0, BTreeSet::len);
        let right_count = component_members.get(right).map_or(0, BTreeSet::len);
        right_count.cmp(&left_count).then_with(|| left.cmp(right))
    });
    if component_ids.len() > plan.max_components {
        record_limit(limiting_resources, QueryResource::Results)?;
    }
    component_ids.truncate(plan.max_components);
    let reported: BTreeSet<String> = component_ids.iter().cloned().collect();

    let (max_responsibility_evidence, max_source_refs) = architecture_detail_limits(plan.detail);
    let mut components: Vec<ArchitectureComponent> = Vec::new();
    for id in &component_ids {
        let Some(members) = component_members.get(id) else {
            continue;
        };
        let mut kinds: BTreeSet<String> = BTreeSet::new();
        let mut files = BTreeSet::new();
        let mut source_refs = Vec::new();
        let mut confidence = 1_000_u16;
        for symbol in members {
            if let Some(kind) = entity_kind.get(symbol) {
                kinds.insert(kind.clone());
            }
            if let Some(file) = entity_file.get(symbol) {
                files.insert(*file);
            }
            if let Some(entity) = find_entity(document, *symbol) {
                confidence = confidence.min(architecture_tier_confidence(entity.tier));
                if source_refs.len() < max_source_refs
                    && let Some(source) = entity.evidence.source.as_ref()
                    && !source_refs.contains(source)
                {
                    source_refs.push(source.clone());
                }
            }
        }
        let mut responsibility_evidence: Vec<String> = Vec::new();
        responsibility_evidence.push("contains_symbols".to_owned());
        for kind in &kinds {
            responsibility_evidence.push(format!("entity_kind:{kind}"));
        }
        responsibility_evidence.truncate(max_responsibility_evidence);
        let component = ArchitectureComponent {
            id: id.clone(),
            kind: component_kind
                .get(id)
                .cloned()
                .unwrap_or_else(|| "unknown".to_owned()),
            name: component_name
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.clone()),
            symbol_count: u32::try_from(members.len()).unwrap_or(u32::MAX),
            file_count: u32::try_from(files.len()).unwrap_or(u32::MAX),
            responsibility_evidence,
            source_refs,
            confidence: if file_granularity {
                files
                    .iter()
                    .filter_map(|file| file_confidence.get(file).copied())
                    .max()
                    .unwrap_or(confidence)
            } else {
                confidence
            },
        };
        emit_cycle_value(
            &mut components,
            component,
            tracker,
            limiting_resources,
            control,
        )?;
    }

    let mut aggregated: BTreeMap<(String, String, RelationFamily), (u32, u16)> = BTreeMap::new();
    for (subject, object, family, confidence) in &raw_edges {
        let (Some(from), Some(to)) = (entity_component.get(subject), entity_component.get(object))
        else {
            continue;
        };
        if from == to || !reported.contains(from) || !reported.contains(to) {
            continue;
        }
        let entry = aggregated
            .entry((from.clone(), to.clone(), *family))
            .or_insert((0, 0));
        entry.0 = entry.0.saturating_add(1);
        if *confidence > entry.1 {
            entry.1 = *confidence;
        }
    }

    let mut connections: Vec<ArchitectureConnection> = Vec::new();
    if plan.include_edges {
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
    }

    // Rank reported components by structural fan-in and fan-out, normalizing
    // the score so the busiest component scores 1000.
    let mut fan_in: BTreeMap<String, u32> = BTreeMap::new();
    let mut fan_out: BTreeMap<String, u32> = BTreeMap::new();
    let mut history_signal: BTreeMap<String, u32> = BTreeMap::new();
    let mut ownership_signal: BTreeMap<String, u32> = BTreeMap::new();
    let mut test_signal: BTreeMap<String, u32> = BTreeMap::new();
    for (from, to, _family) in aggregated.keys() {
        let outbound = fan_out.entry(from.clone()).or_insert(0);
        *outbound = outbound.saturating_add(1);
        let inbound = fan_in.entry(to.clone()).or_insert(0);
        *inbound = inbound.saturating_add(1);
    }
    for ((from, to, family), (weight, _confidence)) in &aggregated {
        let signals = match family {
            RelationFamily::History => Some(&mut history_signal),
            RelationFamily::Ownership => Some(&mut ownership_signal),
            RelationFamily::Tests => Some(&mut test_signal),
            _ => None,
        };
        if let Some(signals) = signals {
            for component in [from, to] {
                let signal = signals.entry(component.clone()).or_insert(0);
                *signal = signal.saturating_add(*weight);
            }
        }
    }
    let max_total = component_ids
        .iter()
        .map(|component| {
            fan_in
                .get(component)
                .copied()
                .unwrap_or(0)
                .saturating_add(fan_out.get(component).copied().unwrap_or(0))
        })
        .max()
        .unwrap_or(0);
    let mut ranked: Vec<(String, u32, u32, u16)> = Vec::new();
    for component in &component_ids {
        let inbound = fan_in.get(component).copied().unwrap_or(0);
        let outbound = fan_out.get(component).copied().unwrap_or(0);
        let total = inbound.saturating_add(outbound);
        if total == 0 {
            continue;
        }
        let score = if max_total == 0 {
            0
        } else {
            u16::try_from(u64::from(total) * 1_000 / u64::from(max_total)).unwrap_or(1_000)
        };
        ranked.push((component.clone(), inbound, outbound, score));
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
    for (component, inbound, outbound, score) in ranked {
        let hotspot = ArchitectureHotspot {
            component_id: component.clone(),
            fan_in: inbound,
            fan_out: outbound,
            change_frequency: history_signal.get(&component).copied(),
            complexity: None,
            ownership_signal: ownership_signal.get(&component).copied(),
            test_signal: test_signal.get(&component).copied(),
            score,
        };
        emit_cycle_value(&mut hotspots, hotspot, tracker, limiting_resources, control)?;
    }

    let mut communities = Vec::new();
    if plan.views.contains(&ArchitectureOverviewView::Communities) {
        for community in build_architecture_communities(&component_ids, &aggregated, control)? {
            emit_cycle_value(
                &mut communities,
                community,
                tracker,
                limiting_resources,
                control,
            )?;
        }
    }

    let mut views: Vec<ArchitectureOverviewDerivedView> = Vec::new();
    for view in &plan.views {
        let derived = ArchitectureOverviewDerivedView {
            view: *view,
            algorithm_version: architecture_overview_algorithm_version(*view).to_owned(),
            parameters: architecture_overview_parameters(*view),
        };
        emit_cycle_value(&mut views, derived, tracker, limiting_resources, control)?;
    }

    Ok(ArchitectureOverviewAnalysis {
        components,
        connections,
        hotspots,
        communities,
        views,
    })
}

/// Served relation families used to relate tests to seed symbols.
///
/// Each family maps to a disjoint IR predicate set, so a served relation
/// contributes to exactly one direct-edge rationale. `CalledBy` is intentionally
/// omitted because it shares the `Calls` predicate and would double-count the
/// same directed edge.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TestsSelectSignal {
    label: &'static str,
    history: bool,
    build: bool,
}

fn tests_select_signal(predicate: RelationPredicate) -> Option<TestsSelectSignal> {
    let (label, history, build) = match predicate {
        RelationPredicate::Calls | RelationPredicate::DispatchCandidate => ("calls", false, false),
        RelationPredicate::RefersTo => ("references", false, false),
        RelationPredicate::UsesType
        | RelationPredicate::ReturnsType
        | RelationPredicate::ParameterType => ("types", false, false),
        RelationPredicate::Implements
        | RelationPredicate::Satisfies
        | RelationPredicate::Extends
        | RelationPredicate::Embeds
        | RelationPredicate::MixesIn
        | RelationPredicate::Overrides => ("implements", false, false),
        RelationPredicate::Imports => ("imports", false, false),
        RelationPredicate::Tests => ("tests", false, false),
        RelationPredicate::DependsOn => ("build_dependency", false, true),
        RelationPredicate::CoChangedWith => ("history", true, false),
        _ => return None,
    };
    Some(TestsSelectSignal {
        label,
        history,
        build,
    })
}

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
    framework: String,
    score: u16,
    why: Vec<String>,
    estimated_cost_ms: u32,
}

struct TestsSelectCandidate {
    kind: TestsSelectKind,
    framework: String,
    estimated_cost_ms: u32,
}

type TestsSelectEdges = BTreeMap<(SymbolId, TestsSelectSignal), (u16, bool)>;
type TestsSelectAdjacency = BTreeMap<SymbolId, TestsSelectEdges>;

fn test_candidate(entity: &rootlight_ir::EntityRecord, path: Option<&str>) -> TestsSelectCandidate {
    let path = path.unwrap_or_default().to_ascii_lowercase();
    let name = entity.display_name.to_ascii_lowercase();
    let joined = format!("{path}/{name}");
    let file_name = path.rsplit('/').next().unwrap_or_default();
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _extension)| stem);
    let conventional_unit_test = name.starts_with("test_")
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
        || stem.ends_with(".test")
        || stem.ends_with(".spec");
    let kind = if joined.contains("e2e") || joined.contains("end_to_end") {
        TestsSelectKind::E2e
    } else if joined.contains("contract") || joined.contains("schema") {
        TestsSelectKind::Contract
    } else if joined.contains("integration")
        || path.contains("/tests/")
        || path.starts_with("tests/")
    {
        TestsSelectKind::Integration
    } else if conventional_unit_test {
        TestsSelectKind::Unit
    } else if joined.contains("lint") || joined.contains("clippy") || joined.contains("static") {
        TestsSelectKind::Static
    } else if joined.contains("build") || joined.contains("compile") {
        TestsSelectKind::Build
    } else {
        TestsSelectKind::Unit
    };
    let language = entity.language.to_ascii_lowercase();
    let framework = if joined.contains("vitest") {
        "vitest"
    } else if joined.contains("jest") {
        "jest"
    } else if language == "rust" {
        "rust_test"
    } else if language == "python" {
        "pytest"
    } else if language == "go" {
        "go_test"
    } else if matches!(
        language.as_str(),
        "javascript" | "typescript" | "tsx" | "jsx"
    ) {
        "javascript_test"
    } else {
        language.as_str()
    }
    .to_owned();
    let estimated_cost_ms = match kind {
        TestsSelectKind::Unit => 1_000,
        TestsSelectKind::Integration | TestsSelectKind::Contract => 5_000,
        TestsSelectKind::Static => 10_000,
        TestsSelectKind::E2e => 30_000,
        TestsSelectKind::Build => 60_000,
    };
    TestsSelectCandidate {
        kind,
        framework,
        estimated_cost_ms,
    }
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
    let file_paths: BTreeMap<FileId, String> = document
        .files
        .iter()
        .map(|file| (file.id, file.path.clone()))
        .collect();
    let requested_seed_paths: BTreeSet<&str> = plan.seed_paths.iter().map(String::as_str).collect();
    let requested_build_targets: BTreeSet<&str> =
        plan.seed_build_targets.iter().map(String::as_str).collect();
    let mut effective_seeds = plan.seeds.clone();
    let mut resolved_seed_paths = BTreeSet::new();
    let mut resolved_build_targets = BTreeSet::new();
    let mut entity_file: BTreeMap<SymbolId, FileId> = BTreeMap::new();
    let mut tests: BTreeMap<SymbolId, TestsSelectCandidate> = BTreeMap::new();
    let mut scan_truncated = false;
    for entity in &document.entities {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            scan_truncated = true;
            break;
        }
        tracker.add_rows(1)?;
        let source_file = entity
            .evidence
            .source
            .as_ref()
            .map(|source| source.span().file());
        if let Some(file) = source_file {
            entity_file.insert(entity.id, file);
            if let Some(path) = file_paths.get(&file)
                && requested_seed_paths.contains(path.as_str())
            {
                effective_seeds.insert(entity.id);
                resolved_seed_paths.insert(path.clone());
            }
        }
        if entity.kind == EntityKind::BuildTarget {
            for target in &requested_build_targets {
                if entity.canonical_name == *target
                    || entity.display_name == *target
                    || entity.qualified_name == *target
                {
                    effective_seeds.insert(entity.id);
                    resolved_build_targets.insert((*target).to_owned());
                }
            }
        }
        if entity_is_test(entity) {
            let path = source_file
                .and_then(|file| file_paths.get(&file))
                .map(String::as_str);
            tests.insert(entity.id, test_candidate(entity, path));
        }
    }

    // A single bounded relation scan contributes explicit test, semantic,
    // build-target, and bounded historical signals without a second graph walk.
    let mut out_adj = TestsSelectAdjacency::new();
    let mut saw_dispatch_candidate = false;
    let mut observed_history_evidence = false;
    let mut observed_build_evidence = false;
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            scan_truncated = true;
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
        let dispatch_candidate = relation.predicate == RelationPredicate::DispatchCandidate;
        saw_dispatch_candidate |= dispatch_candidate;
        let Some(signal) = tests_select_signal(relation.predicate) else {
            continue;
        };
        observed_history_evidence |= signal.history;
        observed_build_evidence |= signal.build;
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        if subject == object {
            continue;
        }
        let confidence = effective_relation_confidence(document, relation);
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            scan_truncated = true;
            break;
        }
        tracker.add_edges(1)?;
        let aggregate = out_adj
            .entry(subject)
            .or_default()
            .entry((object, signal))
            .or_insert((0, false));
        aggregate.0 = aggregate.0.max(confidence);
        aggregate.1 |= dispatch_candidate;
    }
    let coverage = repository_coverage_summary(document, control, tracker, limiting_resources)?;
    scan_truncated |= coverage.truncated;
    let mut negative_coverage_complete = coverage.entities_complete
        && coverage.relations_semantic_complete
        && !scan_truncated
        && !saw_dispatch_candidate;

    // Resolve the file set occupied by the seeds for the co-location signal.
    let mut seed_files: BTreeSet<FileId> = BTreeSet::new();
    for seed in &effective_seeds {
        if let Some(file) = entity_file.get(seed) {
            seed_files.insert(*file);
        }
    }

    let requested_kinds: BTreeSet<TestsSelectKind> = plan.test_kinds.iter().copied().collect();

    // Score every test entity that matches the requested kind filter.
    let mut scored: Vec<TestsSelectScored> = Vec::new();
    let mut any_direct = false;
    let mut any_transitive = false;
    let mut any_history = false;
    let mut any_build = false;
    let mut any_colocated = false;
    let mut covered_seeds: BTreeSet<SymbolId> = BTreeSet::new();
    let requested_frameworks: BTreeSet<&str> = plan.frameworks.iter().map(String::as_str).collect();
    'test_candidates: for (test_id, candidate) in &tests {
        control.check()?;
        if (!requested_kinds.is_empty() && !requested_kinds.contains(&candidate.kind))
            || (!requested_frameworks.is_empty()
                && !requested_frameworks.contains(candidate.framework.as_str()))
        {
            continue;
        }
        // Direct signal: strongest outbound edge into a seed.
        let mut direct_confidence = 0_u16;
        let mut direct_signal: Option<TestsSelectSignal> = None;
        let mut direct_candidate = false;
        if let Some(edges) = out_adj.get(test_id) {
            for ((target, signal), (confidence, candidate)) in edges {
                if !tests_select_charge_edge_work(tracker, limiting_resources, control)? {
                    negative_coverage_complete = false;
                    break 'test_candidates;
                }
                if effective_seeds.contains(target) && *confidence > direct_confidence {
                    direct_confidence = *confidence;
                    direct_signal = Some(*signal);
                    direct_candidate = *candidate;
                    covered_seeds.insert(*target);
                }
            }
        }
        // Transitive signal: strongest two-hop path test -> node -> seed,
        // weighted by the weakest edge on the path.
        let mut transitive_confidence = 0_u16;
        let mut transitive_candidate = false;
        let mut transitive_history = false;
        let mut transitive_build = false;
        if direct_confidence == 0
            && let Some(edges) = out_adj.get(test_id)
        {
            for ((mid, first_signal), (first_confidence, first_candidate)) in edges {
                if !tests_select_charge_edge_work(tracker, limiting_resources, control)? {
                    negative_coverage_complete = false;
                    break 'test_candidates;
                }
                if effective_seeds.contains(mid) {
                    continue;
                }
                let Some(second_hop) = out_adj.get(mid) else {
                    continue;
                };
                for ((target, second_signal), (second_confidence, second_candidate)) in second_hop {
                    if !tests_select_charge_edge_work(tracker, limiting_resources, control)? {
                        negative_coverage_complete = false;
                        break 'test_candidates;
                    }
                    if !effective_seeds.contains(target) {
                        continue;
                    }
                    let path_confidence = (*first_confidence).min(*second_confidence);
                    if path_confidence > transitive_confidence {
                        transitive_confidence = path_confidence;
                        transitive_candidate = *first_candidate || *second_candidate;
                        transitive_history = first_signal.history || second_signal.history;
                        transitive_build = first_signal.build || second_signal.build;
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
            for seed in &effective_seeds {
                if entity_file.get(seed) == Some(test_file) {
                    covered_seeds.insert(*seed);
                }
            }
        }

        let direct = direct_confidence > 0;
        let transitive = transitive_confidence > 0 && !direct;
        if direct {
            any_direct = true;
            if let Some(signal) = direct_signal {
                any_history |= signal.history;
                any_build |= signal.build;
            }
        }
        if transitive {
            any_transitive = true;
            any_history |= transitive_history;
            any_build |= transitive_build;
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
            if let Some(signal) = direct_signal {
                why.push(format!("via:{}", signal.label));
            }
        }
        if transitive {
            why.push("transitive_dependency".to_owned());
        }
        if direct_candidate || transitive_candidate {
            why.push("dispatch_candidate".to_owned());
        }
        if colocated {
            why.push("shared_file_with_seed".to_owned());
        }
        why.truncate(8);
        scored.push(TestsSelectScored {
            test_id: *test_id,
            kind: candidate.kind,
            framework: candidate.framework.clone(),
            score,
            why,
            estimated_cost_ms: candidate.estimated_cost_ms,
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
    let mut selected_cost_ms = 0_u32;
    let mut selected_slow_tests = 0_u16;
    let mut execution_budget_excluded = false;
    for entry in scored {
        if ranked_tests.len() >= plan.max_tests {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        let next_cost_ms = selected_cost_ms
            .checked_add(entry.estimated_cost_ms)
            .ok_or(QueryError::PlanRejected {
                resource: QueryResource::Results,
            })?;
        let is_slow = entry.estimated_cost_ms >= 10_000;
        let next_slow_tests = selected_slow_tests.checked_add(u16::from(is_slow)).ok_or(
            QueryError::PlanRejected {
                resource: QueryResource::Results,
            },
        )?;
        if plan
            .max_total_ms
            .is_some_and(|maximum| next_cost_ms > maximum)
            || plan
                .max_slow_tests
                .is_some_and(|maximum| next_slow_tests > maximum)
        {
            execution_budget_excluded = true;
            continue;
        }
        selected_cost_ms = next_cost_ms;
        selected_slow_tests = next_slow_tests;
        let path = entity_file
            .get(&entry.test_id)
            .and_then(|file| find_file(document, *file))
            .map(|record| record.path.clone());
        let command_hint = plan
            .include_commands
            .then(|| format!("test:{}:{}", entry.framework, entry.test_id));
        let ranked = RankedTestSelection {
            test_id: entry.test_id,
            kind: entry.kind,
            framework: entry.framework,
            path,
            score: entry.score,
            why: entry.why,
            estimated_cost_ms: Some(entry.estimated_cost_ms),
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
            reason: if negative_coverage_complete {
                "no_related_test_observed".to_owned()
            } else {
                "related_test_coverage_incomplete".to_owned()
            },
        };
        emit_cycle_value(&mut gaps, gap, tracker, limiting_resources, control)?;
    }
    for path in &plan.seed_paths {
        if resolved_seed_paths.contains(path) {
            continue;
        }
        if gaps.len() >= TESTS_SELECT_MAX_GAPS {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        emit_cycle_value(
            &mut gaps,
            TestsSelectGap {
                scope: path.clone(),
                reason: "seed_path_not_indexed".to_owned(),
            },
            tracker,
            limiting_resources,
            control,
        )?;
    }
    for target in &plan.seed_build_targets {
        if resolved_build_targets.contains(target) {
            continue;
        }
        if gaps.len() >= TESTS_SELECT_MAX_GAPS {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        emit_cycle_value(
            &mut gaps,
            TestsSelectGap {
                scope: target.clone(),
                reason: "build_target_not_indexed".to_owned(),
            },
            tracker,
            limiting_resources,
            control,
        )?;
    }
    let observed_frameworks: BTreeSet<&str> = tests
        .values()
        .map(|candidate| candidate.framework.as_str())
        .collect();
    for framework in &plan.frameworks {
        if observed_frameworks.contains(framework.as_str()) {
            continue;
        }
        if gaps.len() >= TESTS_SELECT_MAX_GAPS {
            record_limit(limiting_resources, QueryResource::Results)?;
            break;
        }
        emit_cycle_value(
            &mut gaps,
            TestsSelectGap {
                scope: framework.clone(),
                reason: "framework_not_observed".to_owned(),
            },
            tracker,
            limiting_resources,
            control,
        )?;
    }
    if execution_budget_excluded && gaps.len() < TESTS_SELECT_MAX_GAPS {
        emit_cycle_value(
            &mut gaps,
            TestsSelectGap {
                scope: "execution_budget".to_owned(),
                reason: "execution_budget_excluded_candidates".to_owned(),
            },
            tracker,
            limiting_resources,
            control,
        )?;
    }
    if !observed_history_evidence && gaps.len() < TESTS_SELECT_MAX_GAPS {
        emit_cycle_value(
            &mut gaps,
            TestsSelectGap {
                scope: "history_evidence".to_owned(),
                reason: "history_signal_unavailable".to_owned(),
            },
            tracker,
            limiting_resources,
            control,
        )?;
    }
    let build_evidence_requested =
        !plan.seed_build_targets.is_empty() || plan.test_kinds.contains(&TestsSelectKind::Build);
    if build_evidence_requested && !observed_build_evidence && gaps.len() < TESTS_SELECT_MAX_GAPS {
        emit_cycle_value(
            &mut gaps,
            TestsSelectGap {
                scope: "build_evidence".to_owned(),
                reason: "build_target_signal_unavailable".to_owned(),
            },
            tracker,
            limiting_resources,
            control,
        )?;
    }
    let runtime_evidence_observed = document
        .provenance
        .iter()
        .any(|provenance| provenance.producer_kind == ProducerKind::RuntimeTrace);
    if !runtime_evidence_observed && gaps.len() < TESTS_SELECT_MAX_GAPS {
        emit_cycle_value(
            &mut gaps,
            TestsSelectGap {
                scope: "runtime_evidence".to_owned(),
                reason: if saw_dispatch_candidate {
                    "dynamic_dispatch_runtime_evidence_unavailable".to_owned()
                } else {
                    "runtime_coverage_unavailable".to_owned()
                },
            },
            tracker,
            limiting_resources,
            control,
        )?;
    }

    Ok(TestsSelectAnalysis {
        tests: ranked_tests,
        coverage_strategy: TestsSelectCoverage {
            direct_edges: any_direct,
            transitive_signals: any_transitive,
            history_signals: any_history,
            build_target_signals: any_build,
            file_colocation_signals: any_colocated,
        },
        gaps,
    })
}

fn tests_select_charge_edge_work(
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<bool, QueryError> {
    control.check()?;
    if !tracker.can_add(QueryResource::Edges, 1) {
        record_limit(limiting_resources, QueryResource::Edges)?;
        return Ok(false);
    }
    tracker.add_edges(1)?;
    Ok(true)
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

fn change_impact_family(
    policy: ChangeImpactRelationPolicy,
    include_history: bool,
    predicate: RelationPredicate,
) -> Option<RelationFamily> {
    if include_history && predicate == RelationPredicate::CoChangedWith {
        return Some(RelationFamily::History);
    }
    if let Some(family) = predicate_family(CHANGE_IMPACT_FAMILIES, predicate) {
        return Some(family);
    }
    if policy != ChangeImpactRelationPolicy::Conservative {
        return None;
    }
    match predicate {
        RelationPredicate::Tests => Some(RelationFamily::Tests),
        RelationPredicate::DependsOn => Some(RelationFamily::BuildDependency),
        RelationPredicate::CallsRoute | RelationPredicate::ServesRoute => {
            Some(RelationFamily::CallsRoute)
        }
        RelationPredicate::Publishes | RelationPredicate::Consumes => {
            Some(RelationFamily::Messaging)
        }
        RelationPredicate::ReadsTable => Some(RelationFamily::ReadsTable),
        RelationPredicate::WritesTable => Some(RelationFamily::WritesTable),
        RelationPredicate::Reads | RelationPredicate::Writes => Some(RelationFamily::DataFlow),
        RelationPredicate::OwnedBy => Some(RelationFamily::Ownership),
        RelationPredicate::ChangedIn => Some(RelationFamily::History),
        _ => None,
    }
}

fn path_is_in_scope(path: &str, scope_paths: &[String]) -> bool {
    scope_paths.is_empty()
        || scope_paths.iter().any(|scope| {
            path == scope
                || path
                    .strip_prefix(scope)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        })
}

fn label_is_in_scope(label: &str, scopes: &[String]) -> bool {
    scopes.is_empty()
        || scopes.iter().any(|scope| {
            label == scope
                || label
                    .strip_prefix(scope)
                    .is_some_and(|suffix| suffix.starts_with("::"))
        })
}

fn impact_symbol_in_scope(
    plan: &ChangeImpactPlan,
    symbol: SymbolId,
    entity_file: &BTreeMap<SymbolId, FileId>,
    file_path_by_id: &BTreeMap<FileId, String>,
    entity_label: &BTreeMap<SymbolId, String>,
) -> bool {
    let path_matches = plan.scope_paths.is_empty()
        || entity_file
            .get(&symbol)
            .and_then(|file| file_path_by_id.get(file))
            .is_some_and(|path| path_is_in_scope(path, &plan.scope_paths));
    let label = entity_label
        .get(&symbol)
        .map(String::as_str)
        .unwrap_or_default();
    path_matches
        && label_is_in_scope(label, &plan.scope_packages)
        && label_is_in_scope(label, &plan.scope_services)
}

/// Maximum resolved changes reported by one `change.impact`.
const CHANGE_IMPACT_MAX_RESOLVED: usize = 1_256;

/// Maximum test candidates reported by one `change.impact`.
const CHANGE_IMPACT_MAX_TESTS: usize = 500;

// The executor keeps several metadata indexes and one reverse relation graph
// live together. These conservative charges cover B-tree nodes, vector slack,
// and the repository-owned strings cloned into those indexes.
const CHANGE_IMPACT_FIXED_WORKSPACE_BYTES: usize = 64 * 1024;
const CHANGE_IMPACT_FILE_WORKSPACE_BYTES: usize = 256;
const CHANGE_IMPACT_ENTITY_WORKSPACE_BYTES: usize = 512;
const CHANGE_IMPACT_RELATION_WORKSPACE_BYTES: usize = 128;

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
    control.check()?;
    tracker.add_memory(change_impact_workspace_bytes(document)?)?;

    // Resolve per-entity metadata: declaring file, kind label, and public
    // surface membership, plus the path-to-file map used to resolve explicit
    // path changes.
    let file_path_by_id: BTreeMap<FileId, String> = document
        .files
        .iter()
        .map(|file| (file.id, file.path.clone()))
        .collect();
    let mut entity_file: BTreeMap<SymbolId, FileId> = BTreeMap::new();
    let mut entity_kind: BTreeMap<SymbolId, String> = BTreeMap::new();
    let mut entity_label: BTreeMap<SymbolId, String> = BTreeMap::new();
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
        entity_label.insert(entity.id, entity.qualified_name.clone());
        if entity_is_exported(entity) {
            entity_public.insert(entity.id);
        }
    }

    let mut path_to_file: BTreeMap<String, FileId> = BTreeMap::new();
    for file in &document.files {
        path_to_file.insert(file.path.clone(), file.id);
    }

    // A single bounded relation scan selects the requested propagation policy
    // and contributes the reverse dependent adjacency.
    let mut dependents: BTreeMap<SymbolId, Vec<(SymbolId, RelationFamily, u16)>> = BTreeMap::new();
    let mut saw_dispatch_candidate = false;
    let mut saw_history_signal = false;
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
        saw_dispatch_candidate |= relation.predicate == RelationPredicate::DispatchCandidate;
        saw_history_signal |=
            plan.include_history && relation.predicate == RelationPredicate::CoChangedWith;
        let Some(family) = change_impact_family(
            plan.relation_policy,
            plan.include_history,
            relation.predicate,
        ) else {
            continue;
        };
        let confidence = effective_relation_confidence(document, relation);
        if confidence < plan.min_confidence {
            continue;
        }
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        if subject == object {
            continue;
        }
        if !impact_symbol_in_scope(plan, subject, &entity_file, &file_path_by_id, &entity_label) {
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
    let coverage = repository_coverage_summary(document, control, tracker, limiting_resources)?;

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

    let risk_summary = change_impact_risk_summary(
        &resolved_changes,
        &impacted,
        coverage.entities_complete && coverage.relations_semantic_complete && !coverage.truncated,
        saw_dispatch_candidate,
        plan.include_history,
        saw_history_signal,
    );

    Ok(ChangeImpactAnalysis {
        resolved_changes,
        impacted,
        tests,
        risk_summary,
    })
}

fn change_impact_workspace_bytes(document: &NormalizedIrDocument) -> Result<u64, QueryError> {
    let mut bytes = CHANGE_IMPACT_FIXED_WORKSPACE_BYTES
        .saturating_add(
            document
                .files
                .len()
                .saturating_mul(CHANGE_IMPACT_FILE_WORKSPACE_BYTES),
        )
        .saturating_add(
            document
                .entities
                .len()
                .saturating_mul(CHANGE_IMPACT_ENTITY_WORKSPACE_BYTES),
        )
        .saturating_add(
            document
                .relations
                .len()
                .saturating_mul(CHANGE_IMPACT_RELATION_WORKSPACE_BYTES),
        );
    for file in &document.files {
        bytes = bytes.saturating_add(file.path.len().saturating_mul(2));
    }
    for entity in &document.entities {
        bytes = bytes.saturating_add(entity.qualified_name.len());
    }
    checked_usize_to_u64(bytes)
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
    let mut visited = roots.clone();
    let mut frontier: BTreeMap<SymbolId, (u16, Vec<String>)> = roots
        .iter()
        .copied()
        .map(|symbol| (symbol, (1_000, Vec::new())))
        .collect();
    let mut entries = Vec::new();

    for distance in 1..=max_depth {
        control.check()?;
        let remaining = max_dependents.saturating_sub(entries.len());
        let step = bounded_impact_frontier(
            dependents,
            &frontier,
            roots,
            &visited,
            remaining,
            tracker,
            limiting_resources,
            control,
        )?;

        for (symbol, (confidence, via)) in &step.nodes {
            visited.insert(*symbol);
            let entry = ImpactEntryRecord {
                symbol_id: *symbol,
                kind: entity_kind
                    .get(symbol)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_owned()),
                distance,
                confidence: *confidence,
                via: via.clone(),
                is_public: entity_public.contains(symbol),
            };
            emit_cycle_value(&mut entries, entry, tracker, limiting_resources, control)?;
        }
        frontier = step.nodes;

        if step.work_limited {
            return Ok(entries);
        }
        if step.has_more {
            record_limit(limiting_resources, QueryResource::Results)?;
            return Ok(entries);
        }
        if frontier.is_empty() {
            break;
        }

        if distance == max_depth || entries.len() >= max_dependents {
            let probe = bounded_impact_frontier(
                dependents,
                &frontier,
                roots,
                &visited,
                0,
                tracker,
                limiting_resources,
                control,
            )?;
            if probe.has_more {
                let resource = if distance == max_depth {
                    QueryResource::Depth
                } else {
                    QueryResource::Results
                };
                record_limit(limiting_resources, resource)?;
            }
            return Ok(entries);
        }
    }

    Ok(entries)
}

struct BoundedImpactFrontier {
    nodes: BTreeMap<SymbolId, (u16, Vec<String>)>,
    has_more: bool,
    work_limited: bool,
}

type ImpactDependentEdge = (SymbolId, RelationFamily, u16);
type ImpactFrontierSource<'a> = (&'a (u16, Vec<String>), &'a [ImpactDependentEdge]);

#[expect(
    clippy::too_many_arguments,
    reason = "the frontier merge carries the graph, traversal state, and shared query controls"
)]
fn bounded_impact_frontier(
    dependents: &BTreeMap<SymbolId, Vec<(SymbolId, RelationFamily, u16)>>,
    frontier: &BTreeMap<SymbolId, (u16, Vec<String>)>,
    roots: &BTreeSet<SymbolId>,
    visited: &BTreeSet<SymbolId>,
    cap: usize,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<BoundedImpactFrontier, QueryError> {
    let sources: Vec<ImpactFrontierSource<'_>> = frontier
        .iter()
        .filter_map(|(symbol, state)| {
            dependents
                .get(symbol)
                .map(|edges| (state, edges.as_slice()))
        })
        .collect();
    let mut heap = BinaryHeap::new();
    for (source_index, (_, edges)) in sources.iter().enumerate() {
        if let Some((subject, _, _)) = edges.first() {
            heap.push(Reverse((*subject, source_index, 0_usize)));
        }
    }

    let mut nodes = BTreeMap::new();
    let mut has_more = false;
    let mut work_limited = false;
    while let Some(Reverse((subject, source_index, edge_index))) = heap.pop() {
        control.check()?;
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            work_limited = true;
            break;
        }
        tracker.add_edges(1)?;

        let (parent, edges) = sources[source_index];
        let (_, family, edge_confidence) = edges[edge_index];
        let next_index = edge_index.saturating_add(1);
        if let Some((next_subject, _, _)) = edges.get(next_index) {
            heap.push(Reverse((*next_subject, source_index, next_index)));
        }

        if roots.contains(&subject) || visited.contains(&subject) || nodes.contains_key(&subject) {
            continue;
        }
        if nodes.len() >= cap {
            has_more = true;
            break;
        }

        let mut path = parent.1.clone();
        path.push(family.as_str().to_owned());
        path.truncate(16);
        nodes.insert(subject, (parent.0.min(edge_confidence), path));
    }

    Ok(BoundedImpactFrontier {
        nodes,
        has_more,
        work_limited,
    })
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
        seed_paths: Vec::new(),
        seed_build_targets: Vec::new(),
        test_kinds: Vec::new(),
        frameworks: Vec::new(),
        max_tests: CHANGE_IMPACT_MAX_TESTS,
        max_total_ms: None,
        max_slow_tests: None,
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
    coverage_complete: bool,
    saw_dispatch_candidate: bool,
    history_requested: bool,
    history_observed: bool,
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
    if saw_dispatch_candidate || !coverage_complete {
        reasons.push("dynamic_dispatch_blind_spot".to_owned());
    }
    if history_requested {
        reasons.push(
            if history_observed {
                "bounded_history_signal_observed"
            } else {
                "history_signal_unavailable"
            }
            .to_owned(),
        );
    }
    if !coverage_complete {
        reasons.push("impact_coverage_incomplete".to_owned());
    }
    reasons.truncate(16);
    ChangeImpactRiskSummary {
        level,
        reasons,
        coverage: if coverage_complete {
            CoverageStatus::Complete
        } else {
            CoverageStatus::Bounded
        },
        breaking_surface,
        fanout,
        dynamic_blind_spots: saw_dispatch_candidate || !coverage_complete,
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

/// Maximum relation rows inspected while expanding one change-plan target set.
const PLAN_CHANGE_MAX_RELATION_SCAN_ROWS: u64 = 100_000;

/// Maximum rows inspected by each file-target resolution projection.
const PLAN_CHANGE_MAX_FILE_SCAN_ROWS: u64 = 50_000;

/// Maximum rows admitted for the optional whole-generation test projection.
const PLAN_CHANGE_MAX_TEST_SCAN_ROWS: u64 = 50_000;

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
/// Explicit symbols are resolved through the generation's ordered identity
/// index. File targets use a bounded entity scan, and the forward impact closure
/// scans only for dependents of the current breadth-first frontier instead of
/// materializing a repository-wide adjacency map. The reused `tests.select`
/// ranking runs only when its complete projection fits a smaller optional-work
/// allowance. The impact summary, open decisions, and context-pack request
/// remain source-free and honest: omitted work is reported as truncation.
fn build_plan_change(
    document: &NormalizedIrDocument,
    plan: &PlanChangePlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<PlanChangeAnalysis, QueryError> {
    let mut entity_file: BTreeMap<SymbolId, FileId> = BTreeMap::new();
    let mut entity_kind: BTreeMap<SymbolId, String> = BTreeMap::new();
    let mut entity_public: BTreeSet<SymbolId> = BTreeSet::new();
    let mut resolved_targets = plan.target_symbols.clone();
    for symbol in &plan.target_symbols {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        insert_plan_entity_metadata(
            document,
            *symbol,
            &mut entity_file,
            &mut entity_kind,
            &mut entity_public,
        )?;
    }

    let mut resolved_target_files: BTreeSet<FileId> = BTreeSet::new();
    for file in &plan.target_files {
        resolved_target_files.insert(*file);
    }
    for file in &document.files {
        control.check()?;
        if plan.target_paths.iter().any(|path| {
            file.path == *path
                || file
                    .path
                    .strip_prefix(path)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        }) {
            resolved_target_files.insert(file.id);
        }
    }
    if !resolved_target_files.is_empty() {
        let mut scanned_rows = 0_u64;
        for entity in &document.entities {
            control.check()?;
            if scanned_rows >= PLAN_CHANGE_MAX_FILE_SCAN_ROWS
                || !tracker.can_add(QueryResource::Rows, 1)
            {
                record_limit(limiting_resources, QueryResource::Rows)?;
                break;
            }
            tracker.add_rows(1)?;
            scanned_rows = scanned_rows.saturating_add(1);
            let Some(source) = entity.evidence.source.as_ref() else {
                continue;
            };
            if !resolved_target_files.contains(&source.span().file()) {
                continue;
            }
            resolved_targets.insert(entity.id);
            insert_plan_entity_metadata(
                document,
                entity.id,
                &mut entity_file,
                &mut entity_kind,
                &mut entity_public,
            )?;
        }

        // Some normalized entities have containment evidence without a direct
        // source span. Preserve file-target resolution through a separately
        // bounded `Contains` projection.
        scanned_rows = 0;
        for relation in &document.relations {
            control.check()?;
            if scanned_rows >= PLAN_CHANGE_MAX_FILE_SCAN_ROWS
                || !tracker.can_add(QueryResource::Rows, 1)
            {
                record_limit(limiting_resources, QueryResource::Rows)?;
                break;
            }
            tracker.add_rows(1)?;
            scanned_rows = scanned_rows.saturating_add(1);
            if relation.predicate != RelationPredicate::Contains {
                continue;
            }
            let (RelationEndpoint::File(file), RelationEndpoint::Entity(symbol)) =
                (relation.subject, relation.object)
            else {
                continue;
            };
            if !resolved_target_files.contains(&file) {
                continue;
            }
            resolved_targets.insert(symbol);
            entity_file.insert(symbol, file);
            if tracker.can_add(QueryResource::Rows, 1) {
                tracker.add_rows(1)?;
                insert_plan_entity_metadata(
                    document,
                    symbol,
                    &mut entity_file,
                    &mut entity_kind,
                    &mut entity_public,
                )?;
            } else {
                record_limit(limiting_resources, QueryResource::Rows)?;
            }
        }
    }

    if resolved_targets.is_empty() {
        return Err(QueryError::SymbolNotFound);
    }

    let closure = plan_change_impact_closure(
        document,
        &resolved_targets,
        plan.max_depth,
        plan.max_dependents,
        &mut entity_file,
        &mut entity_kind,
        &mut entity_public,
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

    let step_inputs = PlanChangeStepInputs {
        objective_text: &plan.objective_text,
        resolved_targets: &resolved_targets,
        closure: &closure,
        test_symbols: &test_symbols,
        affected_scope: &affected_scope,
        constraints: &plan.constraints,
        max_steps: plan.max_steps,
    };
    let (plan_steps, steps_truncated) = build_plan_change_steps(plan.objective, &step_inputs);
    if steps_truncated {
        record_limit(limiting_resources, QueryResource::Results)?;
    }

    let open_decisions = plan_change_decisions(plan.objective, &affected_scope, &plan.constraints);

    let (context_pack_request, context_pack_truncated) = plan_change_context_pack(
        &resolved_targets,
        &closure,
        &resolved_target_files,
        &entity_file,
    );
    if context_pack_truncated {
        record_limit(limiting_resources, QueryResource::Results)?;
    }

    Ok(PlanChangeAnalysis {
        plan: plan_steps,
        affected_scope,
        test_plan,
        open_decisions,
        context_pack_request,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the target-centered projection updates each bounded metadata view explicitly"
)]
fn plan_change_impact_closure(
    document: &NormalizedIrDocument,
    roots: &BTreeSet<SymbolId>,
    max_depth: u8,
    max_dependents: usize,
    entity_file: &mut BTreeMap<SymbolId, FileId>,
    entity_kind: &mut BTreeMap<SymbolId, String>,
    entity_public: &mut BTreeSet<SymbolId>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<Vec<ImpactEntryRecord>, QueryError> {
    let allowed: BTreeSet<RelationPredicate> = CHANGE_IMPACT_FAMILIES
        .iter()
        .flat_map(|family| family.predicates().iter().copied())
        .collect();
    let mut visited = roots.clone();
    let mut frontier: BTreeMap<SymbolId, (u16, Vec<String>)> = roots
        .iter()
        .copied()
        .map(|symbol| (symbol, (1_000, Vec::new())))
        .collect();
    let mut entries = Vec::new();
    let mut scanned_rows = 0_u64;
    let mut depth_limited = false;

    for distance in 1..=max_depth {
        control.check()?;
        let mut next_frontier: BTreeMap<SymbolId, (u16, Vec<String>)> = BTreeMap::new();
        let mut results_limited = false;
        for relation in &document.relations {
            control.check()?;
            if scanned_rows >= PLAN_CHANGE_MAX_RELATION_SCAN_ROWS
                || !tracker.can_add(QueryResource::Rows, 1)
            {
                record_limit(limiting_resources, QueryResource::Rows)?;
                return Ok(entries);
            }
            tracker.add_rows(1)?;
            scanned_rows = scanned_rows.saturating_add(1);
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
            let Some((parent_confidence, parent_path)) = frontier.get(&object) else {
                continue;
            };
            if subject == object || visited.contains(&subject) {
                continue;
            }
            if !tracker.can_add(QueryResource::Edges, 1) {
                record_limit(limiting_resources, QueryResource::Edges)?;
                return Ok(entries);
            }
            tracker.add_edges(1)?;
            let confidence =
                (*parent_confidence).min(effective_relation_confidence(document, relation));
            let mut path = parent_path.clone();
            path.push(family.as_str().to_owned());
            path.truncate(16);
            let candidate = (confidence, path);
            let dependent_cap_reached =
                entries.len().saturating_add(next_frontier.len()) >= max_dependents;
            match next_frontier.entry(subject) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    if dependent_cap_reached {
                        record_limit(limiting_resources, QueryResource::Results)?;
                        results_limited = true;
                    } else {
                        entry.insert(candidate);
                    }
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if candidate.0 > entry.get().0
                        || (candidate.0 == entry.get().0 && candidate.1 < entry.get().1)
                    {
                        entry.insert(candidate);
                    }
                }
            }
        }

        if next_frontier.is_empty() {
            frontier.clear();
            break;
        }
        let mut retained_frontier = BTreeMap::new();
        for (symbol, (confidence, via)) in next_frontier {
            if entries.len() >= max_dependents {
                record_limit(limiting_resources, QueryResource::Results)?;
                return Ok(entries);
            }
            visited.insert(symbol);
            if tracker.can_add(QueryResource::Rows, 1) {
                tracker.add_rows(1)?;
                insert_plan_entity_metadata(
                    document,
                    symbol,
                    entity_file,
                    entity_kind,
                    entity_public,
                )?;
            } else {
                record_limit(limiting_resources, QueryResource::Rows)?;
            }
            let entry = ImpactEntryRecord {
                symbol_id: symbol,
                kind: entity_kind
                    .get(&symbol)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_owned()),
                distance,
                confidence,
                via: via.clone(),
                is_public: entity_public.contains(&symbol),
            };
            let previous_len = entries.len();
            emit_cycle_value(&mut entries, entry, tracker, limiting_resources, control)?;
            if entries.len() == previous_len {
                return Ok(entries);
            }
            retained_frontier.insert(symbol, (confidence, via));
        }
        frontier = retained_frontier;
        if results_limited {
            return Ok(entries);
        }
        depth_limited = distance == max_depth && !frontier.is_empty();
    }

    if depth_limited {
        record_limit(limiting_resources, QueryResource::Depth)?;
    }
    Ok(entries)
}

fn insert_plan_entity_metadata(
    document: &NormalizedIrDocument,
    symbol: SymbolId,
    entity_file: &mut BTreeMap<SymbolId, FileId>,
    entity_kind: &mut BTreeMap<SymbolId, String>,
    entity_public: &mut BTreeSet<SymbolId>,
) -> Result<(), QueryError> {
    let Some(entity) = find_entity(document, symbol) else {
        return Ok(());
    };
    if let Some(source) = entity.evidence.source.as_ref() {
        entity_file.insert(symbol, source.span().file());
    }
    entity_kind.insert(symbol, serialized_label(&entity.kind)?);
    if entity_is_exported(entity) {
        entity_public.insert(symbol);
    }
    Ok(())
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
                build_target_signals: false,
                file_colocation_signals: false,
            },
            gaps: Vec::new(),
        });
    }
    // The reused selection admits a bounded seed set; keep the smallest
    // identities deterministically when the impacted surface is larger.
    if seeds.len() > PLAN_CHANGE_MAX_CONTEXT_ITEMS {
        seeds = seeds
            .into_iter()
            .take(PLAN_CHANGE_MAX_CONTEXT_ITEMS)
            .collect();
        record_limit(limiting_resources, QueryResource::Results)?;
    }
    let projection_rows = u64::try_from(document.entities.len())
        .unwrap_or(u64::MAX)
        .saturating_add(u64::try_from(document.relations.len()).unwrap_or(u64::MAX));
    if limiting_resources.contains(&QueryResource::Rows)
        || projection_rows > tracker.remaining_rows()
        || projection_rows > PLAN_CHANGE_MAX_TEST_SCAN_ROWS
    {
        record_limit(limiting_resources, QueryResource::Rows)?;
        return Ok(TestsSelectAnalysis {
            tests: Vec::new(),
            coverage_strategy: TestsSelectCoverage {
                direct_edges: false,
                transitive_signals: false,
                history_signals: false,
                build_target_signals: false,
                file_colocation_signals: false,
            },
            gaps: Vec::new(),
        });
    }
    let selection_plan = TestsSelectPlan {
        seeds,
        seed_paths: Vec::new(),
        seed_build_targets: Vec::new(),
        test_kinds: Vec::new(),
        frameworks: Vec::new(),
        max_tests: PLAN_CHANGE_MAX_TESTS,
        max_total_ms: None,
        max_slow_tests: None,
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

struct PlanChangeStepInputs<'a> {
    objective_text: &'a str,
    resolved_targets: &'a BTreeSet<SymbolId>,
    closure: &'a [ImpactEntryRecord],
    test_symbols: &'a [SymbolId],
    affected_scope: &'a PlanChangeImpactSummary,
    constraints: &'a [String],
    max_steps: usize,
}

/// Builds the deterministic ordered plan steps from the objective and impact.
///
/// Modification objectives emit inspect, modify, update-dependents, and
/// run-tests steps plus a public-surface confirmation when public surface is
/// touched; explanation and review objectives emit read-only inspect, trace or
/// assess, and report steps. The first action identifies the caller-authored
/// requested outcome and caller constraints as instructions to validate; risk
/// codes and verification hints remain source-free. The sequence is capped at
/// `max_steps`, and every step only depends on earlier ordinals so truncation
/// keeps dependencies valid.
fn build_plan_change_steps(
    objective: PlanChangeObjective,
    inputs: &PlanChangeStepInputs<'_>,
) -> (Vec<PlanChangeStepRecord>, bool) {
    let PlanChangeStepInputs {
        objective_text,
        resolved_targets,
        closure,
        test_symbols,
        affected_scope,
        constraints,
        max_steps,
    } = inputs;
    let target_symbols_truncated = resolved_targets.len() > PLAN_CHANGE_MAX_STEP_TARGETS;
    let direct_dependents_truncated =
        closure.iter().filter(|entry| entry.distance == 1).count() > PLAN_CHANGE_MAX_STEP_TARGETS;
    let test_targets_truncated = test_symbols.len() > PLAN_CHANGE_MAX_STEP_TARGETS;
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
    if !constraints.is_empty() {
        let step = u8::try_from(steps.len().saturating_add(1)).unwrap_or(100);
        let depends_on = steps
            .last()
            .map(|previous| vec![previous.step])
            .unwrap_or_default();
        let mut summary = constraints.join("; ");
        if summary.chars().count() > 768 {
            summary = summary.chars().take(765).collect::<String>();
            summary.push_str("...");
        }
        steps.push(plan_step(
            step,
            &format!("Verify the planned change against caller constraints: {summary}"),
            target_symbols.clone(),
            depends_on,
            &["constraint_violation"],
            Some("verify every caller-provided constraint before completion"),
        ));
    }
    let truncated = steps.len() > *max_steps
        || target_symbols_truncated
        || direct_dependents_truncated
        || test_targets_truncated;
    steps.truncate(*max_steps);
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
    _constraints: &[String],
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
) -> (PlanChangeContextPack, bool) {
    let mut symbols: BTreeSet<SymbolId> = resolved_targets.clone();
    for entry in closure {
        symbols.insert(entry.symbol_id);
    }
    let symbols_truncated = symbols.len() > PLAN_CHANGE_MAX_CONTEXT_ITEMS;
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
    let files_truncated = files.len() > PLAN_CHANGE_MAX_CONTEXT_ITEMS;
    let files: Vec<FileId> = files
        .into_iter()
        .take(PLAN_CHANGE_MAX_CONTEXT_ITEMS)
        .collect();
    (
        PlanChangeContextPack { symbols, files },
        symbols_truncated || files_truncated,
    )
}

/// Maximum semantic changes, breaking candidates, or lineage matches carried in
/// one `history.compare` result page.
const HISTORY_COMPARE_MAX_RESULTS: usize = 1_000;

/// Maximum change-kind filter categories admitted by one `history.compare` plan.
const HISTORY_COMPARE_MAX_CHANGE_KINDS: usize = 8;

/// Maximum breaking candidates carried in one `history.compare` result.
const HISTORY_COMPARE_MAX_BREAKING: usize = 256;

// Each logical item can occupy several live B-tree indexes during comparison.
// These conservative charges cover the borrowed entry values plus node slack.
const HISTORY_COMPARE_FIXED_WORKSPACE_BYTES: usize = 64 * 1024;
const HISTORY_COMPARE_ENTITY_WORKSPACE_BYTES: usize = 2 * 1024;
const HISTORY_COMPARE_FILE_WORKSPACE_BYTES: usize = 512;
const HISTORY_COMPARE_RELATION_WORKSPACE_BYTES: usize = 512;

/// Comparable per-entity fingerprint used to diff two generations.
///
/// The fingerprint captures the normalized fields needed for identity-preserved
/// comparison and conservative one-to-one rename or move matching.
struct HistoryEntityFingerprint<'a> {
    kind: EntityKind,
    language: &'a str,
    canonical_name: &'a str,
    normalized_name: &'a str,
    container_identity: Option<HistoryContainerIdentity<'a>>,
    is_public: bool,
    source_identity: Option<HistoryFileIdentity<'a>>,
    source_content_hash: Option<ContentHash>,
    source_start: u64,
    source_end: u64,
}

impl<'a> HistoryEntityFingerprint<'a> {
    fn from_entity(
        entity: &'a rootlight_ir::EntityRecord,
        entities: &BTreeMap<SymbolId, &'a rootlight_ir::EntityRecord>,
        file_paths: &BTreeMap<FileId, &'a str>,
    ) -> Self {
        let span = entity.evidence.source.as_ref().map(|source| source.span());
        Self {
            kind: entity.kind,
            language: &entity.language,
            canonical_name: &entity.canonical_name,
            normalized_name: if entity.flags.contains(&EntityFlag::Synthetic) {
                &entity.display_name
            } else {
                &entity.canonical_name
            },
            container_identity: history_container_identity(entity.container, entities, file_paths),
            is_public: entity_is_exported(entity),
            source_identity: span.map(|span| history_file_identity(span.file(), file_paths)),
            source_content_hash: entity.evidence.source.as_ref().map(SourceRef::content_hash),
            source_start: span.map_or(0, |span| span.start_byte()),
            source_end: span.map_or(0, |span| span.end_byte()),
        }
    }

    const fn semantic_key(&self) -> HistorySemanticKey<'a> {
        HistorySemanticKey {
            kind: self.kind,
            language: self.language,
            normalized_name: self.normalized_name,
            container_identity: self.container_identity,
            source_identity: self.source_identity,
        }
    }

    const fn signature_key(&self) -> HistorySignatureKey<'a> {
        HistorySignatureKey {
            language: self.language,
            normalized_name: self.normalized_name,
            container_identity: self.container_identity,
            source_identity: self.source_identity,
        }
    }

    fn surface_differs(&self, other: &Self) -> bool {
        self.kind != other.kind || self.is_public != other.is_public
    }
}

/// Stable file identity used only while comparing generation-local records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HistoryFileIdentity<'a> {
    Path(&'a str),
    Id(FileId),
}

/// Stable container identity used only while comparing generation-local records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HistoryContainerIdentity<'a> {
    Repository,
    File(HistoryFileIdentity<'a>),
    Entity {
        kind: EntityKind,
        language: &'a str,
        qualified_name: &'a str,
    },
    EntityId(SymbolId),
}

/// Normalized semantic identity available in the current common IR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct HistorySemanticKey<'a> {
    kind: EntityKind,
    language: &'a str,
    normalized_name: &'a str,
    container_identity: Option<HistoryContainerIdentity<'a>>,
    source_identity: Option<HistoryFileIdentity<'a>>,
}

/// Declaration identity used to preserve lineage across a kind change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct HistorySignatureKey<'a> {
    language: &'a str,
    normalized_name: &'a str,
    container_identity: Option<HistoryContainerIdentity<'a>>,
    source_identity: Option<HistoryFileIdentity<'a>>,
}

/// Exact fields that prove an entity moved with unchanged source content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct HistoryMoveKey<'a> {
    kind: EntityKind,
    language: &'a str,
    canonical_name: &'a str,
    source_content_hash: ContentHash,
    source_start: u64,
    source_end: u64,
}

/// Stable declaration neighborhood used for conservative rename matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct HistoryRenameKey<'a> {
    kind: EntityKind,
    language: &'a str,
    container_identity: Option<HistoryContainerIdentity<'a>>,
    source_identity: HistoryFileIdentity<'a>,
    source_start: u64,
}

/// Bounded `history.compare` analysis assembled before result emission.
struct HistoryCompareAnalysis {
    coverage: CoverageStatus,
    changes: Vec<SemanticChangeRecord>,
    architecture_delta: HistoryArchitectureDelta,
    breaking_candidates: Vec<BreakingCandidateRecord>,
    lineage: Vec<LineageMatchRecord>,
}

fn history_file_identity<'a>(
    file: FileId,
    file_paths: &BTreeMap<FileId, &'a str>,
) -> HistoryFileIdentity<'a> {
    file_paths
        .get(&file)
        .map_or(HistoryFileIdentity::Id(file), |path| {
            HistoryFileIdentity::Path(path)
        })
}

fn history_container_identity<'a>(
    container: Option<ContainerRef>,
    entities: &BTreeMap<SymbolId, &'a rootlight_ir::EntityRecord>,
    file_paths: &BTreeMap<FileId, &'a str>,
) -> Option<HistoryContainerIdentity<'a>> {
    let identity = match container {
        None => return None,
        Some(ContainerRef::Repository(_)) => HistoryContainerIdentity::Repository,
        Some(ContainerRef::File(file)) => {
            HistoryContainerIdentity::File(history_file_identity(file, file_paths))
        }
        Some(ContainerRef::Entity(symbol)) => {
            if let Some(entity) = entities.get(&symbol) {
                HistoryContainerIdentity::Entity {
                    kind: entity.kind,
                    language: &entity.language,
                    qualified_name: &entity.qualified_name,
                }
            } else {
                HistoryContainerIdentity::EntityId(symbol)
            }
        }
    };
    Some(identity)
}

fn history_source_snapshot(
    document: &NormalizedIrDocument,
) -> Option<BTreeMap<&str, (ContentHash, u64)>> {
    let mut snapshot = BTreeMap::new();
    for file in &document.files {
        if snapshot
            .insert(file.path.as_str(), (file.content_hash, file.byte_length))
            .is_some()
        {
            return None;
        }
    }
    Some(snapshot)
}

fn history_source_snapshots_match(
    base: &NormalizedIrDocument,
    head: &NormalizedIrDocument,
) -> bool {
    base.repository == head.repository
        && history_source_snapshot(base)
            .zip(history_source_snapshot(head))
            .is_some_and(|(base, head)| base == head)
}

/// Builds a bounded semantic comparison between two generation documents.
///
/// The base and head entity sets are indexed by stable identity and diffed into
/// added, removed, modified, moved, and renamed changes. Identity-preserved
/// symbols and uniquely proven moves or renames form lineage matches. Removed,
/// renamed, or modified public-surface symbols become breaking candidates ranked
/// by their base-generation consumer count. Component-boundary and cross-service
/// relation changes contribute an aggregate architecture delta. Rows, edges,
/// results, and memory are bounded exactly like `change.impact`.
fn build_history_compare(
    base_document: &NormalizedIrDocument,
    head_document: &NormalizedIrDocument,
    plan: &HistoryComparePlan,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<HistoryCompareAnalysis, QueryError> {
    control.check()?;
    tracker.add_memory(history_compare_workspace_bytes(
        base_document,
        head_document,
    )?)?;
    let source_snapshots_match = history_source_snapshots_match(base_document, head_document);
    let base_entities = history_entity_index(
        base_document,
        &plan.scope,
        control,
        tracker,
        limiting_resources,
    )?;
    let head_entities = history_entity_index(
        head_document,
        &plan.scope,
        control,
        tracker,
        limiting_resources,
    )?;

    // Union of every observed identity in deterministic order.
    let mut identities: BTreeSet<SymbolId> = BTreeSet::new();
    identities.extend(base_entities.keys().copied());
    identities.extend(head_entities.keys().copied());
    let mut unmatched_base: BTreeSet<SymbolId> = BTreeSet::new();
    let mut unmatched_head: BTreeSet<SymbolId> = BTreeSet::new();

    let mut changes: Vec<SemanticChangeRecord> = Vec::new();
    let mut lineage: Vec<LineageMatchRecord> = Vec::new();
    // Breaking candidates carry their change significance for deterministic
    // ordering; the consumer count is filled after one bounded relation scan.
    let mut breaking: Vec<(u16, BreakingCandidateRecord)> = Vec::new();
    let mut breaking_symbols: BTreeSet<SymbolId> = BTreeSet::new();

    for symbol in identities {
        control.check()?;
        match (base_entities.get(&symbol), head_entities.get(&symbol)) {
            (None, Some(_)) => {
                unmatched_head.insert(symbol);
            }
            (Some(_), None) => {
                unmatched_base.insert(symbol);
            }
            (Some(base), Some(head)) => {
                if !source_snapshots_match && base.surface_differs(head) {
                    let kind = HistorySemanticChangeKind::SignatureModified;
                    let breaking_candidate = base.is_public;
                    let significance = history_significance(kind, breaking_candidate);
                    let change = SemanticChangeRecord {
                        kind,
                        symbol_id: symbol,
                        entity_kind: serialized_label(&head.kind)?,
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
                } else if !source_snapshots_match && base.source_identity != head.source_identity {
                    let kind = HistorySemanticChangeKind::Moved;
                    emit_cycle_value(
                        &mut changes,
                        SemanticChangeRecord {
                            kind,
                            symbol_id: symbol,
                            entity_kind: serialized_label(&head.kind)?,
                            breaking_candidate: false,
                            significance: history_significance(kind, false),
                        },
                        tracker,
                        limiting_resources,
                        control,
                    )?;
                }
            }
            (None, None) => {}
        }
    }

    let semantic_pairs = unique_history_matches(
        &base_entities,
        &head_entities,
        &unmatched_base,
        &unmatched_head,
        |entity| Some(entity.semantic_key()),
        control,
    )?;
    for (base_symbol, head_symbol) in semantic_pairs {
        let base = base_entities
            .get(&base_symbol)
            .ok_or(QueryError::SymbolNotFound)?;
        let head = head_entities
            .get(&head_symbol)
            .ok_or(QueryError::SymbolNotFound)?;
        unmatched_base.remove(&base_symbol);
        unmatched_head.remove(&head_symbol);
        if lineage.len() < plan.max_results {
            emit_cycle_value(
                &mut lineage,
                LineageMatchRecord {
                    base_symbol_id: base_symbol,
                    head_symbol_id: head_symbol,
                    confidence: 1_000,
                    is_rename: false,
                },
                tracker,
                limiting_resources,
                control,
            )?;
        }
        if !source_snapshots_match && base.surface_differs(head) {
            let kind = HistorySemanticChangeKind::SignatureModified;
            let breaking_candidate = base.is_public;
            let significance = history_significance(kind, breaking_candidate);
            emit_cycle_value(
                &mut changes,
                SemanticChangeRecord {
                    kind,
                    symbol_id: head_symbol,
                    entity_kind: serialized_label(&head.kind)?,
                    breaking_candidate,
                    significance,
                },
                tracker,
                limiting_resources,
                control,
            )?;
            if breaking_candidate {
                breaking_symbols.insert(base_symbol);
                breaking.push((
                    significance,
                    BreakingCandidateRecord {
                        symbol_id: base_symbol,
                        consumer_count: 0,
                        is_public_surface: true,
                        reason: "modified_public_surface".to_owned(),
                    },
                ));
            }
        }
    }

    if !source_snapshots_match {
        let signature_pairs = unique_history_matches(
            &base_entities,
            &head_entities,
            &unmatched_base,
            &unmatched_head,
            |entity| Some(entity.signature_key()),
            control,
        )?;
        for (base_symbol, head_symbol) in signature_pairs {
            let base = base_entities
                .get(&base_symbol)
                .ok_or(QueryError::SymbolNotFound)?;
            let head = head_entities
                .get(&head_symbol)
                .ok_or(QueryError::SymbolNotFound)?;
            if !base.surface_differs(head) {
                continue;
            }
            unmatched_base.remove(&base_symbol);
            unmatched_head.remove(&head_symbol);
            let kind = HistorySemanticChangeKind::SignatureModified;
            let breaking_candidate = base.is_public;
            let significance = history_significance(kind, breaking_candidate);
            emit_history_lineage_change(
                &mut changes,
                &mut lineage,
                kind,
                base_symbol,
                head_symbol,
                head,
                breaking_candidate,
                950,
                plan,
                tracker,
                limiting_resources,
                control,
            )?;
            if breaking_candidate {
                breaking_symbols.insert(base_symbol);
                breaking.push((
                    significance,
                    BreakingCandidateRecord {
                        symbol_id: base_symbol,
                        consumer_count: 0,
                        is_public_surface: true,
                        reason: "modified_public_surface".to_owned(),
                    },
                ));
            }
        }

        let move_pairs = unique_history_matches(
            &base_entities,
            &head_entities,
            &unmatched_base,
            &unmatched_head,
            |entity| {
                Some(HistoryMoveKey {
                    kind: entity.kind,
                    language: entity.language,
                    canonical_name: entity.canonical_name,
                    source_content_hash: entity.source_content_hash?,
                    source_start: entity.source_start,
                    source_end: entity.source_end,
                })
            },
            control,
        )?;
        for (base_symbol, head_symbol) in move_pairs {
            let base = base_entities
                .get(&base_symbol)
                .ok_or(QueryError::SymbolNotFound)?;
            let head = head_entities
                .get(&head_symbol)
                .ok_or(QueryError::SymbolNotFound)?;
            if base.source_identity == head.source_identity {
                continue;
            }
            unmatched_base.remove(&base_symbol);
            unmatched_head.remove(&head_symbol);
            emit_history_lineage_change(
                &mut changes,
                &mut lineage,
                HistorySemanticChangeKind::Moved,
                base_symbol,
                head_symbol,
                head,
                false,
                1_000,
                plan,
                tracker,
                limiting_resources,
                control,
            )?;
        }

        let rename_pairs = unique_history_matches(
            &base_entities,
            &head_entities,
            &unmatched_base,
            &unmatched_head,
            |entity| {
                Some(HistoryRenameKey {
                    kind: entity.kind,
                    language: entity.language,
                    container_identity: entity.container_identity,
                    source_identity: entity.source_identity?,
                    source_start: entity.source_start,
                })
            },
            control,
        )?;
        for (base_symbol, head_symbol) in rename_pairs {
            let base = base_entities
                .get(&base_symbol)
                .ok_or(QueryError::SymbolNotFound)?;
            let head = head_entities
                .get(&head_symbol)
                .ok_or(QueryError::SymbolNotFound)?;
            if base.canonical_name == head.canonical_name
                || !history_names_resemble(base.canonical_name, head.canonical_name)
            {
                continue;
            }
            unmatched_base.remove(&base_symbol);
            unmatched_head.remove(&head_symbol);
            let breaking_candidate = base.is_public;
            let significance =
                history_significance(HistorySemanticChangeKind::Renamed, breaking_candidate);
            emit_history_lineage_change(
                &mut changes,
                &mut lineage,
                HistorySemanticChangeKind::Renamed,
                base_symbol,
                head_symbol,
                head,
                breaking_candidate,
                900,
                plan,
                tracker,
                limiting_resources,
                control,
            )?;
            if breaking_candidate {
                breaking_symbols.insert(base_symbol);
                breaking.push((
                    significance,
                    BreakingCandidateRecord {
                        symbol_id: base_symbol,
                        consumer_count: 0,
                        is_public_surface: true,
                        reason: "renamed_public_surface".to_owned(),
                    },
                ));
            }
        }

        for symbol in unmatched_base {
            control.check()?;
            let base = base_entities
                .get(&symbol)
                .ok_or(QueryError::SymbolNotFound)?;
            let kind = HistorySemanticChangeKind::Removed;
            let breaking_candidate = base.is_public;
            let significance = history_significance(kind, breaking_candidate);
            emit_cycle_value(
                &mut changes,
                SemanticChangeRecord {
                    kind,
                    symbol_id: symbol,
                    entity_kind: serialized_label(&base.kind)?,
                    breaking_candidate,
                    significance,
                },
                tracker,
                limiting_resources,
                control,
            )?;
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

        for symbol in unmatched_head {
            control.check()?;
            let head = head_entities
                .get(&symbol)
                .ok_or(QueryError::SymbolNotFound)?;
            let kind = HistorySemanticChangeKind::Added;
            emit_cycle_value(
                &mut changes,
                SemanticChangeRecord {
                    kind,
                    symbol_id: symbol,
                    entity_kind: serialized_label(&head.kind)?,
                    breaking_candidate: false,
                    significance: history_significance(kind, false),
                },
                tracker,
                limiting_resources,
                control,
            )?;
        }
    }

    let relation_change_inputs = HistoryRelationChangeInputs {
        base_document,
        head_document,
        base_entities: &base_entities,
        head_entities: &head_entities,
        plan,
    };
    append_history_relation_changes(
        &relation_change_inputs,
        &mut changes,
        control,
        tracker,
        limiting_resources,
    )?;
    let architecture_delta = if plan.change_kinds.is_empty()
        || plan.change_kinds.contains(&HistoryChangeKind::Architecture)
    {
        let base_scope: BTreeSet<SymbolId> = base_entities.keys().copied().collect();
        let head_scope: BTreeSet<SymbolId> = head_entities.keys().copied().collect();
        history_architecture_delta(
            base_document,
            head_document,
            &base_scope,
            &head_scope,
            control,
            tracker,
            limiting_resources,
        )?
    } else {
        HistoryArchitectureDelta {
            new_cross_service_edges: 0,
            removed_cross_service_edges: 0,
            new_boundaries: 0,
            removed_boundaries: 0,
        }
    };

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
        changes.retain(|change| {
            history_change_matches_filter(change.kind, &change.entity_kind, &plan.change_kinds)
        });
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

    // Changed identities are relevant lineage even when the caller omits
    // unchanged context. Populate those first so a broad unchanged set cannot
    // consume the bounded lineage page before an actual change is represented.
    let changed_identities: BTreeSet<SymbolId> = changes
        .iter()
        .map(|change| change.symbol_id)
        .filter(|symbol| base_entities.contains_key(symbol) && head_entities.contains_key(symbol))
        .collect();
    for symbol in changed_identities {
        if lineage.len() >= plan.max_results
            || lineage
                .iter()
                .any(|match_| match_.base_symbol_id == symbol && match_.head_symbol_id == symbol)
        {
            continue;
        }
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
    if plan.include_unchanged_context {
        for symbol in base_entities
            .keys()
            .filter(|symbol| head_entities.contains_key(symbol))
        {
            if lineage.len() >= plan.max_results
                || lineage.iter().any(|match_| {
                    match_.base_symbol_id == *symbol && match_.head_symbol_id == *symbol
                })
            {
                continue;
            }
            emit_cycle_value(
                &mut lineage,
                LineageMatchRecord {
                    base_symbol_id: *symbol,
                    head_symbol_id: *symbol,
                    confidence: 1_000,
                    is_rename: false,
                },
                tracker,
                limiting_resources,
                control,
            )?;
        }
    }

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

    lineage.sort_by(|left, right| {
        left.base_symbol_id
            .cmp(&right.base_symbol_id)
            .then_with(|| left.head_symbol_id.cmp(&right.head_symbol_id))
    });
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
        // Exact one-to-one moves, local renames, relation deltas, and observed
        // architecture changes are modeled. Dynamic behavior and ambiguous
        // identity changes remain explicit structural-analysis bounds.
        CoverageStatus::Bounded
    };

    Ok(HistoryCompareAnalysis {
        coverage,
        changes,
        architecture_delta,
        breaking_candidates,
        lineage,
    })
}

fn history_compare_workspace_bytes(
    base: &NormalizedIrDocument,
    head: &NormalizedIrDocument,
) -> Result<u64, QueryError> {
    let entities = base.entities.len().saturating_add(head.entities.len());
    let files = base.files.len().saturating_add(head.files.len());
    let relations = base.relations.len().saturating_add(head.relations.len());
    let bytes = HISTORY_COMPARE_FIXED_WORKSPACE_BYTES
        .saturating_add(entities.saturating_mul(HISTORY_COMPARE_ENTITY_WORKSPACE_BYTES))
        .saturating_add(files.saturating_mul(HISTORY_COMPARE_FILE_WORKSPACE_BYTES))
        .saturating_add(relations.saturating_mul(HISTORY_COMPARE_RELATION_WORKSPACE_BYTES));
    checked_usize_to_u64(bytes)
}

struct HistoryRelationChangeInputs<'a, 'document> {
    base_document: &'document NormalizedIrDocument,
    head_document: &'document NormalizedIrDocument,
    base_entities: &'a BTreeMap<SymbolId, HistoryEntityFingerprint<'document>>,
    head_entities: &'a BTreeMap<SymbolId, HistoryEntityFingerprint<'document>>,
    plan: &'a HistoryComparePlan,
}

fn append_history_relation_changes(
    inputs: &HistoryRelationChangeInputs<'_, '_>,
    changes: &mut Vec<SemanticChangeRecord>,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<(), QueryError> {
    let base_scope: BTreeSet<SymbolId> = inputs.base_entities.keys().copied().collect();
    let head_scope: BTreeSet<SymbolId> = inputs.head_entities.keys().copied().collect();
    let base_relations = history_relation_index(
        inputs.base_document,
        &base_scope,
        &inputs.plan.change_kinds,
        control,
        tracker,
        limiting_resources,
    )?;
    let head_relations = history_relation_index(
        inputs.head_document,
        &head_scope,
        &inputs.plan.change_kinds,
        control,
        tracker,
        limiting_resources,
    )?;
    let mut emitted = BTreeSet::new();
    for relation in base_relations
        .difference(&head_relations)
        .chain(head_relations.difference(&base_relations))
    {
        control.check()?;
        let symbol = match relation.1 {
            RelationEndpoint::Entity(symbol) => symbol,
            _ => match relation.2 {
                RelationEndpoint::Entity(symbol) => symbol,
                _ => continue,
            },
        };
        let entity = inputs
            .head_entities
            .get(&symbol)
            .or_else(|| inputs.base_entities.get(&symbol));
        let Some(entity) = entity else {
            continue;
        };
        let kind = history_relation_change_kind(relation.0);
        if !emitted.insert((kind, symbol)) {
            continue;
        }
        emit_cycle_value(
            changes,
            SemanticChangeRecord {
                kind,
                symbol_id: symbol,
                entity_kind: serialized_label(&entity.kind)?,
                breaking_candidate: false,
                significance: history_significance(kind, false),
            },
            tracker,
            limiting_resources,
            control,
        )?;
    }
    Ok(())
}

fn history_relation_index(
    document: &NormalizedIrDocument,
    scoped_entities: &BTreeSet<SymbolId>,
    change_kinds: &BTreeSet<HistoryChangeKind>,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<BTreeSet<(RelationPredicate, RelationEndpoint, RelationEndpoint)>, QueryError> {
    let mut relations = BTreeSet::new();
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            break;
        }
        tracker.add_edges(1)?;
        if !history_relation_matches_filter(relation.predicate, change_kinds) {
            continue;
        }
        let subject_scoped = endpoint_entity(document, relation.subject)
            .is_some_and(|symbol| scoped_entities.contains(&symbol));
        let object_scoped = endpoint_entity(document, relation.object)
            .is_some_and(|symbol| scoped_entities.contains(&symbol));
        if subject_scoped || object_scoped {
            relations.insert((relation.predicate, relation.subject, relation.object));
        }
    }
    Ok(relations)
}

fn history_relation_matches_filter(
    predicate: RelationPredicate,
    filter: &BTreeSet<HistoryChangeKind>,
) -> bool {
    if filter.is_empty() || filter.contains(&HistoryChangeKind::Relations) {
        return true;
    }
    (filter.contains(&HistoryChangeKind::Architecture)
        && matches!(
            predicate,
            RelationPredicate::DependsOn
                | RelationPredicate::CallsRoute
                | RelationPredicate::ServesRoute
                | RelationPredicate::Publishes
                | RelationPredicate::Consumes
                | RelationPredicate::MemberOfView
        ))
        || (filter.contains(&HistoryChangeKind::Ownership)
            && predicate == RelationPredicate::OwnedBy)
        || (filter.contains(&HistoryChangeKind::Tests) && predicate == RelationPredicate::Tests)
        || (filter.contains(&HistoryChangeKind::Routes)
            && matches!(
                predicate,
                RelationPredicate::CallsRoute | RelationPredicate::ServesRoute
            ))
        || (filter.contains(&HistoryChangeKind::Data)
            && matches!(
                predicate,
                RelationPredicate::ReadsTable | RelationPredicate::WritesTable
            ))
}

const fn history_relation_change_kind(predicate: RelationPredicate) -> HistorySemanticChangeKind {
    match predicate {
        RelationPredicate::LineageSplitFrom => HistorySemanticChangeKind::Split,
        RelationPredicate::LineageMergedFrom => HistorySemanticChangeKind::Merged,
        RelationPredicate::DependsOn
        | RelationPredicate::CallsRoute
        | RelationPredicate::ServesRoute
        | RelationPredicate::Publishes
        | RelationPredicate::Consumes
        | RelationPredicate::MemberOfView => HistorySemanticChangeKind::ArchitectureChanged,
        _ => HistorySemanticChangeKind::RelationChanged,
    }
}

fn history_architecture_delta(
    base_document: &NormalizedIrDocument,
    head_document: &NormalizedIrDocument,
    base_scope: &BTreeSet<SymbolId>,
    head_scope: &BTreeSet<SymbolId>,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<HistoryArchitectureDelta, QueryError> {
    let base_boundaries = history_boundaries(base_document, base_scope);
    let head_boundaries = history_boundaries(head_document, head_scope);
    let base_edges = history_cross_service_edges(
        base_document,
        base_scope,
        control,
        tracker,
        limiting_resources,
    )?;
    let head_edges = history_cross_service_edges(
        head_document,
        head_scope,
        control,
        tracker,
        limiting_resources,
    )?;
    Ok(HistoryArchitectureDelta {
        new_cross_service_edges: bounded_history_count(head_edges.difference(&base_edges).count()),
        removed_cross_service_edges: bounded_history_count(
            base_edges.difference(&head_edges).count(),
        ),
        new_boundaries: bounded_history_count(head_boundaries.difference(&base_boundaries).count()),
        removed_boundaries: bounded_history_count(
            base_boundaries.difference(&head_boundaries).count(),
        ),
    })
}

fn history_boundaries(
    document: &NormalizedIrDocument,
    scoped_entities: &BTreeSet<SymbolId>,
) -> BTreeSet<(EntityKind, String)> {
    document
        .entities
        .iter()
        .filter(|entity| {
            scoped_entities.contains(&entity.id)
                && matches!(
                    entity.kind,
                    EntityKind::Module
                        | EntityKind::Namespace
                        | EntityKind::Package
                        | EntityKind::BuildTarget
                        | EntityKind::Service
                        | EntityKind::CommunityView
                )
        })
        .map(|entity| (entity.kind, entity.qualified_name.clone()))
        .collect()
}

fn history_cross_service_edges(
    document: &NormalizedIrDocument,
    scoped_entities: &BTreeSet<SymbolId>,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<BTreeSet<(String, String, RelationPredicate)>, QueryError> {
    let parents = entity_parent_map(document);
    let services: BTreeMap<SymbolId, &str> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Service)
        .map(|entity| (entity.id, entity.qualified_name.as_str()))
        .collect();
    let mut edges = BTreeSet::new();
    for relation in &document.relations {
        control.check()?;
        if !tracker.can_add(QueryResource::Edges, 1) {
            record_limit(limiting_resources, QueryResource::Edges)?;
            break;
        }
        tracker.add_edges(1)?;
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        if !scoped_entities.contains(&subject) && !scoped_entities.contains(&object) {
            continue;
        }
        let Some(subject_service) = history_containing_service(subject, &parents, &services) else {
            continue;
        };
        let Some(object_service) = history_containing_service(object, &parents, &services) else {
            continue;
        };
        if subject_service != object_service {
            edges.insert((
                subject_service.to_owned(),
                object_service.to_owned(),
                relation.predicate,
            ));
        }
    }
    Ok(edges)
}

fn history_containing_service<'a>(
    symbol: SymbolId,
    parents: &BTreeMap<SymbolId, SymbolId>,
    services: &'a BTreeMap<SymbolId, &'a str>,
) -> Option<&'a str> {
    let mut current = symbol;
    let mut visited = BTreeSet::new();
    loop {
        if let Some(service) = services.get(&current) {
            return Some(*service);
        }
        if !visited.insert(current) {
            return None;
        }
        current = parents.get(&current).copied()?;
    }
}

fn bounded_history_count(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX).min(10_000)
}

/// Returns only bidirectionally unique matches for a deterministic key.
///
/// Marking duplicate keys as ambiguous prevents collision-dependent aliases.
fn unique_history_matches<'a, Key, KeyFor>(
    base_entities: &BTreeMap<SymbolId, HistoryEntityFingerprint<'a>>,
    head_entities: &BTreeMap<SymbolId, HistoryEntityFingerprint<'a>>,
    base_symbols: &BTreeSet<SymbolId>,
    head_symbols: &BTreeSet<SymbolId>,
    key_for: KeyFor,
    control: &QueryControl<'_>,
) -> Result<Vec<(SymbolId, SymbolId)>, QueryError>
where
    Key: Ord,
    KeyFor: Fn(&HistoryEntityFingerprint<'a>) -> Option<Key>,
{
    fn insert<Key: Ord>(index: &mut BTreeMap<Key, Option<SymbolId>>, key: Key, symbol: SymbolId) {
        index
            .entry(key)
            .and_modify(|candidate| *candidate = None)
            .or_insert(Some(symbol));
    }

    let mut base_index: BTreeMap<Key, Option<SymbolId>> = BTreeMap::new();
    for symbol in base_symbols {
        control.check()?;
        let Some(entity) = base_entities.get(symbol) else {
            continue;
        };
        if let Some(key) = key_for(entity) {
            insert(&mut base_index, key, *symbol);
        }
    }
    let mut head_index: BTreeMap<Key, Option<SymbolId>> = BTreeMap::new();
    for symbol in head_symbols {
        control.check()?;
        let Some(entity) = head_entities.get(symbol) else {
            continue;
        };
        if let Some(key) = key_for(entity) {
            insert(&mut head_index, key, *symbol);
        }
    }

    let mut matches = Vec::new();
    for (key, base_symbol) in base_index {
        control.check()?;
        let Some(base_symbol) = base_symbol else {
            continue;
        };
        let Some(Some(head_symbol)) = head_index.get(&key) else {
            continue;
        };
        matches.push((base_symbol, *head_symbol));
    }
    Ok(matches)
}

/// Rejects same-location replacements whose names share no meaningful anchor.
///
/// Exact Git or declaration-fingerprint evidence can broaden this conservative
/// fallback later without weakening collision handling.
fn history_names_resemble(base: &str, head: &str) -> bool {
    let shorter = base.chars().count().min(head.chars().count());
    if shorter < 4 {
        return false;
    }
    let common_prefix = base
        .chars()
        .zip(head.chars())
        .take_while(|(left, right)| left == right)
        .count();
    let common_suffix = base
        .chars()
        .rev()
        .zip(head.chars().rev())
        .take_while(|(left, right)| left == right)
        .count();
    common_prefix.max(common_suffix) >= 3_usize.max(shorter / 2)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the helper carries one complete bounded history result row"
)]
fn emit_history_lineage_change(
    changes: &mut Vec<SemanticChangeRecord>,
    lineage: &mut Vec<LineageMatchRecord>,
    kind: HistorySemanticChangeKind,
    base_symbol: SymbolId,
    head_symbol: SymbolId,
    head: &HistoryEntityFingerprint<'_>,
    breaking_candidate: bool,
    confidence: u16,
    plan: &HistoryComparePlan,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<(), QueryError> {
    emit_cycle_value(
        changes,
        SemanticChangeRecord {
            kind,
            symbol_id: head_symbol,
            entity_kind: serialized_label(&head.kind)?,
            breaking_candidate,
            significance: history_significance(kind, breaking_candidate),
        },
        tracker,
        limiting_resources,
        control,
    )?;
    if lineage.len() < plan.max_results {
        emit_cycle_value(
            lineage,
            LineageMatchRecord {
                base_symbol_id: base_symbol,
                head_symbol_id: head_symbol,
                confidence,
                is_rename: kind == HistorySemanticChangeKind::Renamed,
            },
            tracker,
            limiting_resources,
            control,
        )?;
    }
    Ok(())
}

/// Indexes one generation's entities by stable identity under the row budget.
fn history_entity_index<'a>(
    document: &'a NormalizedIrDocument,
    scope: &HistoryCompareScope,
    control: &QueryControl<'_>,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
) -> Result<BTreeMap<SymbolId, HistoryEntityFingerprint<'a>>, QueryError> {
    let scoped = history_scope_entities(document, scope);
    let entities: BTreeMap<SymbolId, &rootlight_ir::EntityRecord> = document
        .entities
        .iter()
        .map(|entity| (entity.id, entity))
        .collect();
    let file_paths: BTreeMap<FileId, &str> = document
        .files
        .iter()
        .map(|file| (file.id, file.path.as_str()))
        .collect();
    let mut index: BTreeMap<SymbolId, HistoryEntityFingerprint<'a>> = BTreeMap::new();
    for entity in &document.entities {
        control.check()?;
        if !scoped.contains(&entity.id) {
            continue;
        }
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        index.insert(
            entity.id,
            HistoryEntityFingerprint::from_entity(entity, &entities, &file_paths),
        );
    }
    Ok(index)
}

fn history_scope_entities(
    document: &NormalizedIrDocument,
    scope: &HistoryCompareScope,
) -> BTreeSet<SymbolId> {
    if scope.paths.is_empty()
        && scope.packages.is_empty()
        && scope.services.is_empty()
        && scope.symbols.is_empty()
    {
        return document.entities.iter().map(|entity| entity.id).collect();
    }

    let parents = entity_parent_map(document);
    let mut roots = scope.symbols.clone();
    for entity in &document.entities {
        let package_match =
            entity.kind == EntityKind::Package && entity_matches_label(entity, &scope.packages);
        let service_match =
            entity.kind == EntityKind::Service && entity_matches_label(entity, &scope.services);
        if package_match || service_match {
            roots.insert(entity.id);
        }
    }
    document
        .entities
        .iter()
        .filter(|entity| {
            entity_descends_from(entity.id, &roots, &parents)
                || entity
                    .evidence
                    .source
                    .as_ref()
                    .and_then(|source| find_file(document, source.span().file()))
                    .is_some_and(|file| path_matches_scope(&file.path, &scope.paths))
        })
        .map(|entity| entity.id)
        .collect()
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
        HistorySemanticChangeKind::Renamed => 650,
        HistorySemanticChangeKind::SignatureModified => 600,
        HistorySemanticChangeKind::Moved => 500,
        HistorySemanticChangeKind::Modified => 400,
        HistorySemanticChangeKind::ArchitectureChanged => 550,
        HistorySemanticChangeKind::Split | HistorySemanticChangeKind::Merged => 650,
        HistorySemanticChangeKind::RelationChanged => 300,
        HistorySemanticChangeKind::Added => 200,
    };
    let boosted = if breaking_candidate { base + 300 } else { base };
    if boosted > 1_000 { 1_000 } else { boosted }
}

/// Returns whether one semantic change kind satisfies the change-kind filter.
fn history_change_matches_filter(
    kind: HistorySemanticChangeKind,
    entity_kind: &str,
    filter: &BTreeSet<HistoryChangeKind>,
) -> bool {
    match kind {
        HistorySemanticChangeKind::Added
        | HistorySemanticChangeKind::Removed
        | HistorySemanticChangeKind::Moved
        | HistorySemanticChangeKind::Renamed
        | HistorySemanticChangeKind::Modified => {
            filter.contains(&HistoryChangeKind::Entities)
                || (filter.contains(&HistoryChangeKind::Tests) && entity_kind == "test")
                || (filter.contains(&HistoryChangeKind::Routes)
                    && matches!(entity_kind, "route" | "service" | "message_topic"))
                || (filter.contains(&HistoryChangeKind::Data) && entity_kind == "database_object")
                || (filter.contains(&HistoryChangeKind::Architecture)
                    && matches!(
                        entity_kind,
                        "module"
                            | "namespace"
                            | "package"
                            | "build_target"
                            | "service"
                            | "community_view"
                    ))
        }
        HistorySemanticChangeKind::SignatureModified => {
            filter.contains(&HistoryChangeKind::Entities)
                || filter.contains(&HistoryChangeKind::Signatures)
                || (filter.contains(&HistoryChangeKind::Tests) && entity_kind == "test")
                || (filter.contains(&HistoryChangeKind::Routes)
                    && matches!(entity_kind, "route" | "service" | "message_topic"))
                || (filter.contains(&HistoryChangeKind::Data) && entity_kind == "database_object")
        }
        HistorySemanticChangeKind::RelationChanged => {
            filter.contains(&HistoryChangeKind::Relations)
                || filter.contains(&HistoryChangeKind::Ownership)
                || filter.contains(&HistoryChangeKind::Tests)
                || filter.contains(&HistoryChangeKind::Routes)
                || filter.contains(&HistoryChangeKind::Data)
        }
        HistorySemanticChangeKind::ArchitectureChanged => {
            filter.contains(&HistoryChangeKind::Relations)
                || filter.contains(&HistoryChangeKind::Architecture)
                || filter.contains(&HistoryChangeKind::Routes)
        }
        HistorySemanticChangeKind::Split | HistorySemanticChangeKind::Merged => {
            filter.contains(&HistoryChangeKind::Entities)
                || filter.contains(&HistoryChangeKind::Relations)
        }
    }
}

fn cycle_level_entity_kind(level: CycleProjectionLevel) -> Option<&'static [EntityKind]> {
    match level {
        CycleProjectionLevel::Symbol => None,
        CycleProjectionLevel::Module => Some(&[EntityKind::Module, EntityKind::Namespace]),
        CycleProjectionLevel::Package => Some(&[EntityKind::Package]),
        CycleProjectionLevel::BuildTarget => Some(&[EntityKind::BuildTarget]),
        CycleProjectionLevel::Service => Some(&[EntityKind::Service]),
    }
}

fn cycle_projection_node(
    document: &NormalizedIrDocument,
    parents: &BTreeMap<SymbolId, SymbolId>,
    symbol: SymbolId,
    level: CycleProjectionLevel,
) -> Option<SymbolId> {
    let Some(kinds) = cycle_level_entity_kind(level) else {
        return Some(symbol);
    };
    let mut cursor = symbol;
    let mut visited = BTreeSet::new();
    loop {
        let entity = find_entity(document, cursor)?;
        if kinds.contains(&entity.kind) {
            return Some(cursor);
        }
        if !visited.insert(cursor) {
            return None;
        }
        cursor = parents.get(&cursor).copied()?;
    }
}

fn cycle_adjacency_workspace_bytes(document: &NormalizedIrDocument) -> Result<u64, QueryError> {
    checked_usize_to_u64(
        CYCLE_ADJACENCY_FIXED_WORKSPACE_BYTES
            .saturating_add(
                document
                    .entities
                    .len()
                    .saturating_mul(CYCLE_ADJACENCY_ENTITY_WORKSPACE_BYTES),
            )
            .saturating_add(
                document
                    .relations
                    .len()
                    .saturating_mul(CYCLE_ADJACENCY_RELATION_WORKSPACE_BYTES),
            ),
    )
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
) -> Result<(BTreeMap<SymbolId, Vec<CycleAdjEdge>>, u32), QueryError> {
    control.check()?;
    tracker.add_memory(cycle_adjacency_workspace_bytes(document)?)?;

    let allowed: BTreeSet<RelationPredicate> = plan
        .families
        .iter()
        .flat_map(|family| family.predicates().iter().copied())
        .collect();
    let mut adjacency: BTreeMap<SymbolId, Vec<CycleAdjEdge>> = BTreeMap::new();
    if allowed.is_empty() {
        return Ok((adjacency, 0));
    }
    let scoped_entities = analysis_scope_entities(document, plan.scope.as_ref());
    let parents = entity_parent_map(document);
    let mut omitted = BTreeSet::new();
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
        let confidence = effective_relation_confidence(document, relation);
        if confidence < plan.min_confidence {
            continue;
        }
        let Some(family) = predicate_family(&plan.families, relation.predicate) else {
            continue;
        };
        let Some(mut subject_entity) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(mut object_entity) = endpoint_entity(document, relation.object) else {
            continue;
        };
        if family == RelationFamily::CalledBy {
            std::mem::swap(&mut subject_entity, &mut object_entity);
        }
        if !scoped_entities.contains(&subject_entity) || !scoped_entities.contains(&object_entity) {
            continue;
        }
        let Some(subject) = cycle_projection_node(document, &parents, subject_entity, plan.level)
        else {
            omitted.insert(subject_entity);
            continue;
        };
        let Some(object) = cycle_projection_node(document, &parents, object_entity, plan.level)
        else {
            omitted.insert(object_entity);
            continue;
        };
        if plan.level != CycleProjectionLevel::Symbol && subject == object {
            continue;
        }
        let source_refs: Vec<SourceRef> = relation.evidence.source.iter().cloned().collect();
        adjacency.entry(subject).or_default().push(CycleAdjEdge {
            target: object,
            family,
            confidence,
            source_refs,
        });
    }
    for edges in adjacency.values_mut() {
        control.check()?;
        edges.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| left.family.as_str().cmp(right.family.as_str()))
                .then_with(|| right.confidence.cmp(&left.confidence))
        });
    }
    control.check()?;
    Ok((adjacency, u32::try_from(omitted.len()).unwrap_or(u32::MAX)))
}

/// Detects strongly connected components, representative cycles, and break
/// candidates over the served adjacency view.
///
/// Components are reported when their size clears `min_size` (always at least
/// two), plus size-one self-cycles when explicitly requested. One bounded
/// representative minimal cycle and one cheapest break candidate are extracted
/// per reported component, all under the result and memory budgets.
type CycleDetection = (Vec<CycleComponent>, Vec<CyclePath>, Vec<CycleBreak>);

fn cycle_detection_workspace_bytes(
    adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
) -> Result<u64, QueryError> {
    let node_upper_bound = adjacency.len().saturating_add(
        adjacency
            .values()
            .map(Vec::len)
            .fold(0_usize, usize::saturating_add),
    );
    checked_usize_to_u64(
        CYCLE_DETECTION_FIXED_WORKSPACE_BYTES
            .saturating_add(node_upper_bound.saturating_mul(CYCLE_DETECTION_NODE_WORKSPACE_BYTES)),
    )
}

fn detect_cycles(
    adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
    plan: &ArchitectureCyclesPlan,
    tracker: &mut UsageTracker,
    limiting_resources: &mut Vec<QueryResource>,
    control: &QueryControl<'_>,
) -> Result<CycleDetection, QueryError> {
    control.check()?;
    tracker.add_memory(cycle_detection_workspace_bytes(adjacency)?)?;

    let mut nodes: BTreeSet<SymbolId> = BTreeSet::new();
    for (source, edges) in adjacency {
        control.check()?;
        nodes.insert(*source);
        for edge in edges {
            control.check()?;
            nodes.insert(edge.target);
        }
    }
    let raw_components = strongly_connected_components(adjacency, &nodes, control)?;

    #[derive(Debug)]
    struct RankedCycleComponent {
        members: Vec<SymbolId>,
        internal_edges: u32,
        edge_weight: u64,
        change_risk: u32,
        break_cost: u16,
    }

    let mut selected: Vec<RankedCycleComponent> = Vec::new();
    for mut component in raw_components {
        control.check()?;
        component.sort();
        let size = component.len();
        let self_cycle = if plan.include_self_cycles && size == 1 {
            let node = component[0];
            best_edge(adjacency, node, node, control)?.is_some()
        } else {
            false
        };
        if (size >= 2 && size >= usize::from(plan.min_size)) || self_cycle {
            let member_set: BTreeSet<SymbolId> = component.iter().copied().collect();
            let mut internal_edges = 0_u32;
            let mut edge_weight = 0_u64;
            let mut change_risk = 0_u32;
            let mut minimum_confidence = u16::MAX;
            for member in &member_set {
                control.check()?;
                if let Some(edges) = adjacency.get(member) {
                    for edge in edges {
                        control.check()?;
                        if member_set.contains(&edge.target) {
                            internal_edges = internal_edges.saturating_add(1);
                            edge_weight = edge_weight.saturating_add(u64::from(edge.confidence));
                            change_risk = change_risk
                                .saturating_add(u32::from(edge.family == RelationFamily::History));
                            minimum_confidence = minimum_confidence.min(edge.confidence);
                        }
                    }
                }
            }
            selected.push(RankedCycleComponent {
                members: component,
                internal_edges,
                edge_weight,
                change_risk,
                break_cost: if minimum_confidence == u16::MAX {
                    0
                } else {
                    minimum_confidence
                },
            });
        }
    }
    selected.sort_by(|left, right| {
        let primary = match plan.rank_by {
            CycleRankBy::Size => right.members.len().cmp(&left.members.len()),
            CycleRankBy::EdgeWeight => right.edge_weight.cmp(&left.edge_weight),
            CycleRankBy::ChangeRisk => right.change_risk.cmp(&left.change_risk),
            CycleRankBy::BreakCost => right.break_cost.cmp(&left.break_cost),
        };
        primary
            .then_with(|| right.members.len().cmp(&left.members.len()))
            .then_with(|| left.members[0].cmp(&right.members[0]))
    });
    control.check()?;
    if selected.len() > plan.max_cycles {
        selected.truncate(plan.max_cycles);
        record_limit(limiting_resources, QueryResource::Results)?;
    }

    let mut components: Vec<CycleComponent> = Vec::new();
    let mut cycles: Vec<CyclePath> = Vec::new();
    let mut break_candidates: Vec<CycleBreak> = Vec::new();

    for component in &selected {
        control.check()?;
        let member_set: BTreeSet<SymbolId> = component.members.iter().copied().collect();
        let component_record = CycleComponent {
            size: u32::try_from(component.members.len()).unwrap_or(u32::MAX),
            members: component.members.clone(),
            internal_edges: component.internal_edges,
            edge_weight: component.edge_weight,
            change_risk: component.change_risk,
            break_cost: component.break_cost,
        };
        emit_cycle_value(
            &mut components,
            component_record,
            tracker,
            limiting_resources,
            control,
        )?;

        let cycle_nodes = if component.members.len() == 1 {
            let node = component.members[0];
            vec![node, node]
        } else {
            match representative_cycle(adjacency, &member_set, component.members[0], control)? {
                Some(path) => path,
                None => continue,
            }
        };
        let (confidence, edge_evidence) = cycle_details(adjacency, &cycle_nodes, control)?;
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

        if let Some(break_record) = break_candidate(adjacency, &cycle_nodes, control)? {
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
    control: &QueryControl<'_>,
) -> Result<Vec<Vec<SymbolId>>, QueryError> {
    let mut index = 0_u32;
    let mut indices: BTreeMap<SymbolId, u32> = BTreeMap::new();
    let mut lowlinks: BTreeMap<SymbolId, u32> = BTreeMap::new();
    let mut stack: Vec<SymbolId> = Vec::new();
    let mut on_stack: BTreeSet<SymbolId> = BTreeSet::new();
    let mut components: Vec<Vec<SymbolId>> = Vec::new();

    for start in nodes {
        control.check()?;
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
            control.check()?;
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
    Ok(components)
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
    control: &QueryControl<'_>,
) -> Result<Option<Vec<SymbolId>>, QueryError> {
    let mut parent: BTreeMap<SymbolId, SymbolId> = BTreeMap::new();
    let mut visited: BTreeSet<SymbolId> = BTreeSet::from([start]);
    let mut queue: VecDeque<SymbolId> = VecDeque::from([start]);
    while let Some(node) = queue.pop_front() {
        control.check()?;
        let neighbors = adjacency.get(&node).map(Vec::as_slice).unwrap_or(&[]);
        for edge in neighbors {
            control.check()?;
            let target = edge.target;
            if target == node || !member_set.contains(&target) {
                continue;
            }
            if target == start {
                let mut chain = vec![node];
                let mut cursor = node;
                while cursor != start {
                    let Some(next) = parent.get(&cursor) else {
                        return Ok(None);
                    };
                    cursor = *next;
                    chain.push(cursor);
                }
                chain.reverse();
                chain.push(start);
                return Ok(Some(chain));
            }
            if visited.insert(target) {
                parent.insert(target, node);
                queue.push_back(target);
            }
        }
    }
    Ok(None)
}

/// Returns the strongest edge from one node to another, deterministically.
fn best_edge<'adjacency>(
    adjacency: &'adjacency BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
    from: SymbolId,
    to: SymbolId,
    control: &QueryControl<'_>,
) -> Result<Option<&'adjacency CycleAdjEdge>, QueryError> {
    let Some(edges) = adjacency.get(&from) else {
        return Ok(None);
    };
    let mut selected: Option<&CycleAdjEdge> = None;
    for edge in edges {
        control.check()?;
        if edge.target != to {
            continue;
        }
        let replace = selected.is_none_or(|current| {
            edge.confidence > current.confidence
                || (edge.confidence == current.confidence
                    && edge.family.as_str() > current.family.as_str())
        });
        if replace {
            selected = Some(edge);
        }
    }
    Ok(selected)
}

/// Computes the weakest-edge confidence and bounded evidence for a cycle.
fn cycle_details(
    adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
    nodes: &[SymbolId],
    control: &QueryControl<'_>,
) -> Result<(u16, Vec<SourceRef>), QueryError> {
    const MAX_CYCLE_EVIDENCE: usize = 64;
    let mut confidence = u16::MAX;
    let mut evidence: Vec<SourceRef> = Vec::new();
    for pair in nodes.windows(2) {
        control.check()?;
        if let Some(edge) = best_edge(adjacency, pair[0], pair[1], control)? {
            confidence = confidence.min(edge.confidence);
            for source in &edge.source_refs {
                control.check()?;
                if evidence.len() < MAX_CYCLE_EVIDENCE {
                    evidence.push(source.clone());
                }
            }
        }
    }
    if confidence == u16::MAX {
        confidence = 0;
    }
    Ok((confidence, evidence))
}

/// Selects the cheapest single edge whose removal breaks the cycle.
///
/// Lower confidence means a weaker, cheaper-to-break dependency, so the
/// lowest-confidence cycle edge is proposed and its confidence becomes the
/// break cost.
fn break_candidate(
    adjacency: &BTreeMap<SymbolId, Vec<CycleAdjEdge>>,
    nodes: &[SymbolId],
    control: &QueryControl<'_>,
) -> Result<Option<CycleBreak>, QueryError> {
    const MAX_BREAK_REFS: usize = 8;
    let mut chosen: Option<(SymbolId, SymbolId, &CycleAdjEdge)> = None;
    for pair in nodes.windows(2) {
        control.check()?;
        let (from, to) = (pair[0], pair[1]);
        if let Some(edge) = best_edge(adjacency, from, to, control)? {
            let better = chosen.is_none_or(|(_, _, current)| edge.confidence < current.confidence);
            if better {
                chosen = Some((from, to, edge));
            }
        }
    }
    Ok(chosen.map(|(from, to, edge)| CycleBreak {
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
    }))
}

/// Relation families whose served predicates back the static reachability graph.
///
/// The first-slice oracle records direct calls as `DispatchCandidate`
/// occurrences rather than exact entity-to-entity calls. The `Calls` family
/// admits those candidates, but coverage and confidence retain their
/// uncertainty; occurrence endpoints contribute only when their enclosing
/// entity is known.
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
    /// Whether repository-wide semantic relationship coverage is exhaustive.
    relationship_coverage_complete: bool,
    /// Whether the relation scan was cut short by a row or edge budget.
    truncated: bool,
}

/// Honest result of the bounded static reachability analysis.
struct DeadAnalysis {
    candidates: Vec<DeadCodeCandidate>,
    entry_points: CodeDeadEntryPointSummary,
    blind_spots: Vec<CodeDeadBlindSpot>,
    suppression_rules: Vec<CodeDeadSuppressionRule>,
    coverage_caveats: Vec<String>,
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
    let allowed: BTreeSet<RelationPredicate> = CODE_DEAD_FAMILIES
        .iter()
        .flat_map(|family| family.predicates().iter().copied())
        .collect();

    let mut graph = DeadGraph::default();
    let scoped_entities = analysis_scope_entities(document, plan.scope.as_ref());
    let mut saw_dispatch_candidate = false;
    for entity in &document.entities {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            graph.truncated = true;
            break;
        }
        tracker.add_rows(1)?;
        if scoped_entities.contains(&entity.id) {
            graph.nodes.insert(entity.id);
        }
    }
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
        saw_dispatch_candidate |= relation.predicate == RelationPredicate::DispatchCandidate;
        let confidence = effective_relation_confidence(document, relation);
        if confidence < plan.min_confidence {
            continue;
        }
        let Some(subject) = endpoint_entity(document, relation.subject) else {
            continue;
        };
        let Some(object) = endpoint_entity(document, relation.object) else {
            continue;
        };
        if !scoped_entities.contains(&subject) || !scoped_entities.contains(&object) {
            continue;
        }
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
    let coverage = repository_coverage_summary(document, control, tracker, limiting_resources)?;
    graph.truncated |= coverage.truncated;
    graph.relationship_coverage_complete =
        coverage.relations_semantic_complete && !graph.truncated && !saw_dispatch_candidate;
    for edges in graph.adjacency.values_mut() {
        edges.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| right.confidence.cmp(&left.confidence))
        });
    }
    Ok(graph)
}

/// Resolves the partial entry-point model and classifies each unobserved graph symbol.
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
    let mut generated: BTreeSet<SymbolId> = BTreeSet::new();
    let mut external: BTreeSet<SymbolId> = BTreeSet::new();
    let mut application: BTreeSet<SymbolId> = BTreeSet::new();
    let mut framework: BTreeSet<SymbolId> = BTreeSet::new();
    for entity in &document.entities {
        control.check()?;
        if !tracker.can_add(QueryResource::Rows, 1) {
            record_limit(limiting_resources, QueryResource::Rows)?;
            break;
        }
        tracker.add_rows(1)?;
        if !graph.nodes.contains(&entity.id) {
            continue;
        }
        if entity_is_exported(entity) {
            exported.insert(entity.id);
        }
        if entity_is_test(entity) {
            tests.insert(entity.id);
        }
        if entity.flags.contains(&EntityFlag::Generated) {
            generated.insert(entity.id);
        }
        if entity.flags.contains(&EntityFlag::External)
            || matches!(entity.kind, EntityKind::ExternalSymbol)
        {
            external.insert(entity.id);
        }
        if entity_is_application_entry_point(entity) {
            application.insert(entity.id);
        }
        if entity_is_framework_entry_point(entity) {
            framework.insert(entity.id);
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
            if matches!(
                relation.predicate,
                RelationPredicate::ServesRoute
                    | RelationPredicate::CallsRoute
                    | RelationPredicate::Publishes
                    | RelationPredicate::Consumes
            ) {
                if let Some(subject) = endpoint_entity(document, relation.subject)
                    && graph.nodes.contains(&subject)
                {
                    framework.insert(subject);
                }
                if let Some(object) = endpoint_entity(document, relation.object)
                    && graph.nodes.contains(&object)
                {
                    framework.insert(object);
                }
            }
            continue;
        }
        if let Some(symbol) = endpoint_entity(document, relation.object) {
            exported.insert(symbol);
        }
    }

    let mut entry_points: BTreeSet<SymbolId> = BTreeSet::new();
    let mut exported_suppressed = 0_u32;
    let mut test_suppressed = 0_u32;
    let mut generated_suppressed = 0_u32;
    let mut external_suppressed = 0_u32;
    match plan.entry_point_policy {
        CodeDeadEntryPointPolicy::Standard => {
            entry_points.extend(application.iter().copied());
            entry_points.extend(framework.iter().copied());
        }
        CodeDeadEntryPointPolicy::Library => {}
        CodeDeadEntryPointPolicy::Application => {
            entry_points.extend(application.iter().copied());
        }
        CodeDeadEntryPointPolicy::FrameworkSpecific => {
            entry_points.extend(framework.iter().copied());
        }
        CodeDeadEntryPointPolicy::Explicit => {
            entry_points.extend(
                plan.explicit_entry_points
                    .iter()
                    .copied()
                    .filter(|symbol| graph.nodes.contains(symbol)),
            );
        }
    }
    let policy_entry_points = entry_points.clone();
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
    for symbol in &generated {
        if entry_points.insert(*symbol) {
            generated_suppressed = generated_suppressed.saturating_add(1);
        }
    }
    for symbol in &external {
        if entry_points.insert(*symbol) {
            external_suppressed = external_suppressed.saturating_add(1);
        }
    }

    let candidates = detect_dead_candidates(
        document,
        graph,
        &entry_points,
        &exported,
        &tests,
        &generated,
        &external,
        plan.max_candidates,
        tracker,
        limiting_resources,
        control,
    )?;

    let entry_point_count = u32::try_from(policy_entry_points.len()).unwrap_or(u32::MAX);
    let entry_symbols: Vec<SymbolId> = policy_entry_points.iter().copied().take(64).collect();
    let analysis = DeadAnalysis {
        candidates,
        entry_points: CodeDeadEntryPointSummary {
            policy: plan.entry_point_policy,
            entry_point_count,
            entry_symbols,
            // The first-slice entry-point model is always partial: dynamic
            // dispatch, reflection, and unindexed entry points are not provably
            // resolved.
            complete: false,
        },
        blind_spots: dead_blind_spots(plan, document, graph),
        suppression_rules: dead_suppression_rules(
            exported_suppressed,
            test_suppressed,
            generated_suppressed,
            external_suppressed,
            entry_point_count,
        ),
        coverage_caveats: dead_coverage_caveats(document, graph),
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

/// Classifies every graph symbol not observed from the partial entry-point set.
///
/// The forward closure marks every symbol observed from the protected roots;
/// each remaining graph symbol becomes a review observation ordered by stable
/// identity and capped at `max_candidates`. The classification reports only
/// served static-edge evidence and never asserts runtime liveness.
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
    generated: &BTreeSet<SymbolId>,
    external: &BTreeSet<SymbolId>,
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
    candidate_symbols.sort_by(|left, right| {
        dead_candidate_classification(graph, *right)
            .1
            .cmp(&dead_candidate_classification(graph, *left).1)
            .then_with(|| left.cmp(right))
    });

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
        let (classification, confidence) = dead_candidate_classification(graph, symbol);
        let mut why = Vec::new();
        if incoming == 0 {
            why.push("no_incoming_references".to_owned());
        }
        why.push("not_observed_from_partial_entry_points".to_owned());
        if !graph.relationship_coverage_complete {
            why.push("incoming_relation_coverage_incomplete".to_owned());
        }
        let mut uncertainty =
            vec!["static_reachability_does_not_prove_runtime_liveness".to_owned()];
        if !graph.relationship_coverage_complete {
            uncertainty.push("relationship_coverage_incomplete".to_owned());
        }
        let candidate = DeadCodeCandidate {
            symbol_id: symbol,
            classification,
            confidence,
            why,
            suppressions_checked: suppressions_checked_for(
                symbol, exported, tests, generated, external,
            ),
            reachability: DeadCodeReachabilitySummary {
                reached_from_entry_points: false,
                incoming_edges: incoming,
                strongest_incoming_confidence: max_confidence,
            },
            uncertainty,
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

fn dead_candidate_classification(
    graph: &DeadGraph,
    symbol: SymbolId,
) -> (DeadCodeClassification, u16) {
    let incoming = graph.incoming_count.get(&symbol).copied().unwrap_or(0);
    let strongest = graph
        .incoming_max_confidence
        .get(&symbol)
        .copied()
        .unwrap_or(0);
    let classification = if incoming == 0 {
        DeadCodeClassification::NoObservedIncomingReferences
    } else if strongest >= 500 {
        DeadCodeClassification::NotObservedFromEntryPointsStrongReferences
    } else {
        DeadCodeClassification::NotObservedFromEntryPointsWeakReferences
    };
    let confidence = match (classification, graph.relationship_coverage_complete) {
        (DeadCodeClassification::NoObservedIncomingReferences, true) => 850,
        (DeadCodeClassification::NoObservedIncomingReferences, false) => 300,
        (DeadCodeClassification::NotObservedFromEntryPointsStrongReferences, true) => 700,
        (DeadCodeClassification::NotObservedFromEntryPointsStrongReferences, false) => 500,
        (DeadCodeClassification::NotObservedFromEntryPointsWeakReferences, true) => 400,
        (DeadCodeClassification::NotObservedFromEntryPointsWeakReferences, false) => 250,
    };
    (classification, confidence)
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

fn entity_is_application_entry_point(entity: &rootlight_ir::EntityRecord) -> bool {
    matches!(
        entity.kind,
        EntityKind::Service | EntityKind::Route | EntityKind::BuildTarget
    ) || matches!(entity.canonical_name.as_str(), "main" | "__main__" | "Main")
}

fn entity_is_framework_entry_point(entity: &rootlight_ir::EntityRecord) -> bool {
    matches!(
        entity.kind,
        EntityKind::Service | EntityKind::Route | EntityKind::MessageTopic
    )
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
    generated: &BTreeSet<SymbolId>,
    external: &BTreeSet<SymbolId>,
) -> Vec<String> {
    let mut checked = vec![
        "entry_point".to_owned(),
        "exported".to_owned(),
        "test".to_owned(),
        "generated".to_owned(),
        "external".to_owned(),
    ];
    if exported.contains(&symbol) {
        checked.push("exported_match_included".to_owned());
    }
    if tests.contains(&symbol) {
        checked.push("test_match_included".to_owned());
    }
    if generated.contains(&symbol) {
        checked.push("generated_match_included".to_owned());
    }
    if external.contains(&symbol) {
        checked.push("external_match_included".to_owned());
    }
    checked
}

/// Builds the deterministic source-free blind-spot caveats for the analysis.
fn dead_blind_spots(
    plan: &CodeDeadPlan,
    document: &NormalizedIrDocument,
    graph: &DeadGraph,
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
        category: "reflection".to_owned(),
        affected_count: 0,
    });
    blind_spots.push(CodeDeadBlindSpot {
        category: "dynamic_loading".to_owned(),
        affected_count: 0,
    });
    blind_spots.push(CodeDeadBlindSpot {
        category: "macros".to_owned(),
        affected_count: 0,
    });
    blind_spots.push(CodeDeadBlindSpot {
        category: "excluded_generated_code".to_owned(),
        affected_count: u32::try_from(
            document
                .entities
                .iter()
                .filter(|entity| entity.flags.contains(&EntityFlag::Generated))
                .count(),
        )
        .unwrap_or(u32::MAX),
    });
    blind_spots.push(CodeDeadBlindSpot {
        category: "runtime_registration".to_owned(),
        affected_count: 0,
    });
    blind_spots.push(CodeDeadBlindSpot {
        category: "partial_entry_point_model".to_owned(),
        affected_count: 0,
    });
    if !graph.relationship_coverage_complete {
        blind_spots.push(CodeDeadBlindSpot {
            category: "incomplete_relationship_coverage".to_owned(),
            affected_count: u32::try_from(graph.nodes.len()).unwrap_or(u32::MAX),
        });
    }
    if matches!(
        plan.entry_point_policy,
        CodeDeadEntryPointPolicy::Application
    ) {
        blind_spots.push(CodeDeadBlindSpot {
            category: "application_entry_points".to_owned(),
            affected_count: 0,
        });
    }
    if graph.truncated {
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
    generated_suppressed: u32,
    external_suppressed: u32,
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
        CodeDeadSuppressionRule {
            rule: "generated".to_owned(),
            suppressed_count: generated_suppressed,
        },
        CodeDeadSuppressionRule {
            rule: "external".to_owned(),
            suppressed_count: external_suppressed,
        },
    ]
}

fn dead_coverage_caveats(document: &NormalizedIrDocument, graph: &DeadGraph) -> Vec<String> {
    let mut caveats = vec!["runtime_reachability_unobserved".to_owned()];
    if document
        .entities
        .iter()
        .any(|entity| matches!(entity.tier, AnalysisTier::TierC | AnalysisTier::TierD))
    {
        caveats.push("bounded_or_syntax_only_language_tiers".to_owned());
    }
    if !graph.relationship_coverage_complete {
        caveats.push("relationship_coverage_incomplete".to_owned());
    }
    if graph.truncated {
        caveats.push("budget_truncated_scan".to_owned());
    }
    caveats
}

/// Records one emitted reachability observation under the result and memory budgets.
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

fn occurrence_targets_symbol(
    occurrence: &rootlight_ir::OccurrenceRecord,
    symbol: SymbolId,
) -> bool {
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
    // An empty locate result has no narrower scope. Its negative claim must
    // therefore retain repository-wide coverage instead of inheriting
    // completeness from an empty coverage projection.
    let repository_wide = symbols.is_empty() && files.is_empty();
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
            CoverageScope::File(file) => repository_wide || files.contains(&file),
            CoverageScope::Entity(symbol) => repository_wide || symbols.contains(&symbol),
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

    const fn remaining_rows(&self) -> u64 {
        self.budget.max_rows.saturating_sub(self.rows)
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
            scope: None,
            level: CycleProjectionLevel::Symbol,
            min_confidence: 0,
            min_size,
            max_cycles,
            include_self_cycles,
            rank_by: CycleRankBy::Size,
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
    fn architecture_cycles_rejects_unfunded_detection_workspace() {
        let (a, b) = (symbol(1), symbol(2));
        let adjacency =
            BTreeMap::from([(a, vec![cycle_edge(b, 900)]), (b, vec![cycle_edge(a, 900)])]);
        let mut plan = cycle_plan(2, 50, false);
        plan.budget = QueryBudget::new().with_max_memory_bytes(1);
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);

        assert!(matches!(
            detect_cycles(
                &adjacency,
                &plan,
                &mut tracker,
                &mut limiting_resources,
                &control,
            ),
            Err(QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit: 1,
            })
        ));
        assert_eq!(tracker.memory_bytes, 0);
        assert!(limiting_resources.is_empty());
    }

    #[test]
    fn architecture_cycles_rejects_unfunded_adjacency_before_scanning() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/b.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 2, EntityKind::Function);
        add_calls(&mut document, 110, 11, 12, 900);

        let mut plan = cycle_plan(2, 50, false);
        plan.budget = QueryBudget::new().with_max_memory_bytes(1);
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);

        assert!(matches!(
            build_cycle_adjacency(
                &document,
                &plan,
                &control,
                &mut tracker,
                &mut limiting_resources,
            ),
            Err(QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit: 1,
            })
        ));
        assert_eq!(tracker.memory_bytes, 0);
        assert_eq!(tracker.rows, 0);
        assert_eq!(tracker.edges, 0);
        assert!(limiting_resources.is_empty());
    }

    #[test]
    fn architecture_cycles_scc_observes_an_expired_deadline() {
        let (a, b) = (symbol(1), symbol(2));
        let adjacency =
            BTreeMap::from([(a, vec![cycle_edge(b, 900)]), (b, vec![cycle_edge(a, 900)])]);
        let nodes = BTreeSet::from([a, b]);
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::ZERO);

        assert!(matches!(
            strongly_connected_components(&adjacency, &nodes, &control),
            Err(QueryError::Cancelled(CancellationReason::DeadlineExceeded))
        ));
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

    /// Builds a directed static reachability graph from `(subject, object, confidence)`.
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
        let generated = BTreeSet::new();
        let external = BTreeSet::new();
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
            &generated,
            &external,
            max_candidates,
            &mut tracker,
            &mut limiting_resources,
            &control,
        )
        .expect("bounded static reachability analysis succeeds");
        let execution = authoritative_execution(&limiting_resources);
        (candidates, execution)
    }

    #[test]
    fn code_dead_separates_observed_from_unobserved_symbols() {
        let (entry, a, b, c, d) = (symbol(1), symbol(2), symbol(3), symbol(4), symbol(5));
        // entry -> a -> b is observed; c -> d is not observed from the partial roots.
        let graph = dead_graph(&[(entry, a, 900), (a, b, 900), (c, d, 900)]);
        let entry_points = BTreeSet::from([entry]);
        let candidates = run_dead(&graph, &entry_points, 50);

        let ids: Vec<SymbolId> = candidates.iter().map(|c| c.symbol_id).collect();
        assert_eq!(ids, vec![d, c]);
        assert_eq!(
            candidates[0].classification,
            DeadCodeClassification::NotObservedFromEntryPointsStrongReferences
        );
        assert_eq!(
            candidates[1].classification,
            DeadCodeClassification::NoObservedIncomingReferences
        );
    }

    #[test]
    fn code_dead_reports_no_observed_incoming_references() {
        let (entry, a, b, c) = (symbol(1), symbol(2), symbol(3), symbol(4));
        // entry -> a is observed; b -> c is outside the partial-root closure.
        let graph = dead_graph(&[(entry, a, 900), (b, c, 900)]);
        let entry_points = BTreeSet::from([entry]);
        let candidates = run_dead(&graph, &entry_points, 50);

        let observation = candidates
            .iter()
            .find(|candidate| candidate.symbol_id == b)
            .expect("the no-incoming symbol is reported");
        assert_eq!(
            observation.classification,
            DeadCodeClassification::NoObservedIncomingReferences
        );
        assert_eq!(observation.confidence, 300);
        assert!(
            observation
                .why
                .contains(&"no_incoming_references".to_owned())
        );
        assert!(
            observation
                .why
                .contains(&"not_observed_from_partial_entry_points".to_owned())
        );
        assert!(
            observation
                .why
                .contains(&"incoming_relation_coverage_incomplete".to_owned())
        );
    }

    #[test]
    fn code_dead_keeps_complete_relationship_negatives_below_exact_confidence() {
        let (entry, reached, candidate, referenced) = (symbol(1), symbol(2), symbol(3), symbol(4));
        let mut graph = dead_graph(&[(entry, reached, 900), (candidate, referenced, 900)]);
        graph.relationship_coverage_complete = true;

        let candidates = run_dead(&graph, &BTreeSet::from([entry]), 50);
        let observation = candidates
            .iter()
            .find(|item| item.symbol_id == candidate)
            .expect("the complete-coverage negative is reported");

        assert_eq!(
            observation.classification,
            DeadCodeClassification::NoObservedIncomingReferences
        );
        assert_eq!(observation.confidence, 850);
        assert!(
            !observation
                .why
                .contains(&"incoming_relation_coverage_incomplete".to_owned())
        );
    }

    #[test]
    fn code_dead_reports_unobserved_weak_incoming_references() {
        let (entry, a, b, c) = (symbol(1), symbol(2), symbol(3), symbol(4));
        // entry -> a is observed; b -> c is outside the partial-root closure,
        // and c has only a weak observed incoming edge.
        let graph = dead_graph(&[(entry, a, 900), (b, c, 100)]);
        let entry_points = BTreeSet::from([entry]);
        let candidates = run_dead(&graph, &entry_points, 50);

        let suspected = candidates
            .iter()
            .find(|candidate| candidate.symbol_id == c)
            .expect("the weakly referenced symbol is reported");
        assert_eq!(
            suspected.classification,
            DeadCodeClassification::NotObservedFromEntryPointsWeakReferences
        );
        assert_eq!(suspected.confidence, 250);
    }

    #[test]
    fn code_dead_excludes_entry_point_symbols_and_their_callees() {
        let (a, b, d, e) = (symbol(1), symbol(2), symbol(4), symbol(5));
        let graph = dead_graph(&[(a, b, 900), (d, e, 900)]);

        // With only `a` protected, the d -> e island is dead.
        let without = run_dead(&graph, &BTreeSet::from([a]), 50);
        let ids: Vec<SymbolId> = without.iter().map(|c| c.symbol_id).collect();
        assert_eq!(ids, vec![e, d]);

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
        assert_eq!(
            candidates[0].symbol_id,
            d.min(f),
            "equally ranked candidates use stable identity as the tie-breaker"
        );
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
        assert!(first.windows(2).all(|pair| {
            pair[0].confidence > pair[1].confidence
                || (pair[0].confidence == pair[1].confidence
                    && pair[0].symbol_id < pair[1].symbol_id)
        }));
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

    fn add_file_with_content(
        document: &mut NormalizedIrDocument,
        byte: u8,
        content_byte: u8,
        path: &str,
    ) {
        add_file(document, byte, path);
        document
            .files
            .last_mut()
            .expect("file was just pushed")
            .content_hash = ContentHash::from_bytes([content_byte; 32]);
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

    #[expect(
        clippy::too_many_arguments,
        reason = "the helper exposes every source field varied by history fixtures"
    )]
    fn add_history_entity(
        document: &mut NormalizedIrDocument,
        byte: u8,
        file_byte: u8,
        content_byte: u8,
        start_byte: u64,
        end_byte: u64,
        canonical_name: &str,
        kind: EntityKind,
    ) {
        let source = SourceRef::new(
            document.repository,
            document.generation,
            SourceSpan::new(file_id(file_byte), start_byte, end_byte)
                .expect("test span is ordered"),
            ContentHash::from_bytes([content_byte; 32]),
            None,
        );
        document.entities.push(EntityRecord {
            id: symbol(byte),
            repository: document.repository,
            generation: document.generation,
            kind,
            language: "rust".to_owned(),
            tier: AnalysisTier::TierD,
            canonical_name: canonical_name.to_owned(),
            display_name: canonical_name.to_owned(),
            qualified_name: canonical_name.to_owned(),
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

    fn add_complete_repository_coverage(
        document: &mut NormalizedIrDocument,
        byte: u8,
        domain: FactDomain,
        discovered: u64,
    ) {
        document.coverage_records.push(CoverageRecord {
            id: FactId::from_bytes([byte; 20]),
            repository: document.repository,
            generation: document.generation,
            scope: CoverageScope::Repository(document.repository),
            domain,
            tier: AnalysisTier::TierB,
            status: CoverageStatus::Complete,
            discovered,
            indexed: discovered,
            skipped: 0,
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

    fn add_dispatch_candidate(
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
            RelationPredicate::DispatchCandidate,
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
            scope: None,
            detail: ArchitectureOverviewDetail::Standard,
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
    fn empty_locate_scope_retains_file_coverage_for_repository_negative_claims() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/malformed.rs");
        document.coverage_records.push(CoverageRecord {
            id: FactId::from_bytes([2; 20]),
            repository: document.repository,
            generation: document.generation,
            scope: CoverageScope::File(file_id(1)),
            domain: FactDomain::Entities,
            tier: AnalysisTier::TierB,
            status: CoverageStatus::Unknown,
            discovered: 1,
            indexed: 0,
            skipped: 1,
            provenance: FactId::from_bytes([3; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        });
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(1));
        let mut tracker = UsageTracker::new(QueryBudget::new());
        let mut limiting_resources = Vec::new();

        let (coverage, truncated) = collect_coverage_partial(
            &document,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &mut tracker,
            &control,
            &mut limiting_resources,
        )
        .expect("repository-wide coverage projection succeeds");

        assert!(!truncated);
        assert!(limiting_resources.is_empty());
        assert_eq!(coverage, document.coverage_records);
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
    fn architecture_overview_rejects_unfunded_workspace_before_scanning() {
        let mut document = overview_document();
        for byte in 1..=3 {
            add_file(&mut document, byte, &format!("src/f{byte}.rs"));
            add_entity(&mut document, 10 + byte, byte, EntityKind::Function);
            add_contains(&mut document, 100 + byte, byte, 10 + byte, 800);
        }
        add_calls(&mut document, 110, 13, 11, 900);

        let mut plan = overview_plan(1, true, 0, Vec::new());
        let workspace = architecture_overview_workspace_bytes(&document, &plan)
            .expect("workspace size is representable");
        let larger_result_plan = overview_plan(50, true, 0, Vec::new());
        assert_eq!(
            workspace,
            architecture_overview_workspace_bytes(&document, &larger_result_plan)
                .expect("workspace size remains representable")
        );
        plan.budget = QueryBudget::new().with_max_memory_bytes(workspace - 1);
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);

        assert!(matches!(
            build_architecture_overview(
                &document,
                &plan,
                &control,
                &mut tracker,
                &mut limiting_resources,
            ),
            Err(QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit,
            }) if limit == workspace - 1
        ));
        assert_eq!(tracker.memory_bytes, 0);
        assert_eq!(tracker.rows, 0);
        assert_eq!(tracker.edges, 0);
        assert!(limiting_resources.is_empty());
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
        assert_eq!(
            overview.views[0].parameters.get("score_range"),
            Some(&"0..1000".to_owned())
        );
    }

    #[test]
    fn architecture_overview_builds_deterministic_non_ownership_communities() {
        let mut document = overview_document();
        for byte in 1..=4 {
            add_file(&mut document, byte, &format!("src/f{byte}.rs"));
            add_entity(&mut document, 10 + byte, byte, EntityKind::Function);
            add_contains(&mut document, 100 + byte, byte, 10 + byte, 800);
        }
        add_calls(&mut document, 110, 11, 12, 900);
        add_calls(&mut document, 111, 13, 14, 900);

        let plan = overview_plan(50, false, 0, vec![ArchitectureOverviewView::Communities]);
        let first = run_overview(&document, &plan);
        let second = run_overview(&document, &plan);

        assert!(first.connections.is_empty());
        assert_eq!(first.communities, second.communities);
        assert_eq!(first.communities.len(), 2);
        let member_sets: BTreeSet<Vec<String>> = first
            .communities
            .iter()
            .map(|community| community.members.clone())
            .collect();
        assert_eq!(
            member_sets,
            BTreeSet::from([
                vec![file_id(1).to_string(), file_id(2).to_string()],
                vec![file_id(3).to_string(), file_id(4).to_string()],
            ])
        );
        assert!(first.communities.iter().all(
            |community| !community.ownership_truth && community.internal_connection_weight == 1
        ));
        assert_eq!(
            first.views[0].algorithm_version,
            "weighted_label_propagation_v1"
        );
        assert_eq!(
            first.views[0].parameters.get("ownership_truth"),
            Some(&"not_claimed".to_owned())
        );
        assert_eq!(
            first.views[0].parameters.get("max_iterations"),
            Some(&ARCHITECTURE_COMMUNITY_MAX_ITERATIONS.to_string())
        );
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
            seed_paths: Vec::new(),
            seed_build_targets: Vec::new(),
            test_kinds,
            frameworks: Vec::new(),
            max_tests,
            max_total_ms: None,
            max_slow_tests: None,
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

    fn has_test_gap(selection: &TestsSelectAnalysis, reason: &str) -> bool {
        selection.gaps.iter().any(|gap| gap.reason == reason)
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
            Some(format!("test:rust_test:{}", symbol(21)).as_str())
        );
        assert_eq!(selection.tests[0].estimated_cost_ms, Some(1_000));
        // The co-located test ranks second on the fixed co-location floor.
        assert_eq!(selection.tests[1].test_id, symbol(22));
        assert_eq!(selection.tests[1].score, 150);
        assert_eq!(selection.tests[1].path.as_deref(), Some("src/seed.rs"));
        assert!(
            selection.tests[1]
                .why
                .contains(&"shared_file_with_seed".to_owned())
        );
        // Both static signals are reported used; unavailable evidence remains explicit.
        assert!(selection.coverage_strategy.direct_edges);
        assert!(selection.coverage_strategy.file_colocation_signals);
        assert!(!selection.coverage_strategy.transitive_signals);
        assert!(!selection.coverage_strategy.history_signals);
        assert!(has_test_gap(&selection, "history_signal_unavailable"));
        assert!(has_test_gap(&selection, "runtime_coverage_unavailable"));
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
        assert!(has_test_gap(&selection, "history_signal_unavailable"));
        assert!(has_test_gap(&selection, "runtime_coverage_unavailable"));
    }

    #[test]
    fn tests_select_deduplicates_and_budgets_transitive_edges() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_file(&mut document, 2, "src/t.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);
        add_entity(&mut document, 21, 2, EntityKind::Test);
        for offset in 0_u8..16 {
            add_calls(&mut document, 100 + offset, 21, 12, 800);
            add_calls(&mut document, 140 + offset, 12, 11, 600);
        }

        let plan = tests_select_plan(BTreeSet::from([symbol(11)]), Vec::new(), 20, false);
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);

        let selection = build_tests_select(
            &document,
            &plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )
        .expect("deduplicated test selection succeeds");

        assert_eq!(selection.tests.len(), 1);
        assert_eq!(selection.tests[0].test_id, symbol(21));
        // The 32 source facts are scanned once, then only three unique-edge
        // scoring steps remain: direct, first hop, and second hop.
        assert_eq!(tracker.edges, 35);
        assert!(limiting_resources.is_empty());

        let mut bounded_plan = plan;
        bounded_plan.budget = QueryBudget::new().with_max_edges(34);
        let mut bounded_tracker = UsageTracker::new(bounded_plan.budget);
        let mut bounded_resources = Vec::new();
        let bounded_control = QueryControl::new(&cancellation, bounded_plan.budget.max_duration);
        let bounded_selection = build_tests_select(
            &document,
            &bounded_plan,
            &bounded_control,
            &mut bounded_tracker,
            &mut bounded_resources,
        )
        .expect("edge exhaustion returns a bounded partial selection");

        assert!(bounded_selection.tests.is_empty());
        assert_eq!(bounded_tracker.edges, 34);
        assert_eq!(bounded_resources, vec![QueryResource::Edges]);
    }

    #[test]
    fn tests_select_uses_flagged_tests_and_weak_dispatch_candidates() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/seed.rs");
        add_file(&mut document, 2, "src/flagged_test.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 21, 2, EntityKind::Function);
        document
            .entities
            .last_mut()
            .expect("flagged test entity was just pushed")
            .flags
            .push(EntityFlag::Test);
        add_dispatch_candidate(&mut document, 110, 21, 11, 900);

        let plan = tests_select_plan(BTreeSet::from([symbol(11)]), Vec::new(), 20, false);
        let selection = run_tests_select(&document, &plan);

        assert_eq!(selection.tests.len(), 1);
        assert_eq!(selection.tests[0].test_id, symbol(21));
        assert_eq!(selection.tests[0].kind, TestsSelectKind::Unit);
        // Tier-D dispatch is capped at 399 before entering the direct band.
        assert_eq!(selection.tests[0].score, 819);
        assert!(
            selection.tests[0]
                .why
                .contains(&"direct_test_edge".to_owned())
        );
        assert!(
            selection.tests[0]
                .why
                .contains(&"dispatch_candidate".to_owned())
        );
        assert!(selection.coverage_strategy.direct_edges);
        assert!(has_test_gap(&selection, "history_signal_unavailable"));
        assert!(has_test_gap(
            &selection,
            "dynamic_dispatch_runtime_evidence_unavailable"
        ));
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
        // The relation/test domains have no repository-wide completeness
        // evidence, so the absent edge is qualified rather than exhaustive.
        assert_eq!(selection.gaps.len(), 3);
        assert!(selection.gaps.iter().any(|gap| {
            gap.scope == symbol(12).to_string() && gap.reason == "related_test_coverage_incomplete"
        }));
        assert!(has_test_gap(&selection, "history_signal_unavailable"));
        assert!(has_test_gap(&selection, "runtime_coverage_unavailable"));
    }

    #[test]
    fn tests_select_qualifies_an_exhaustive_gap_with_complete_coverage() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        document
            .entities
            .last_mut()
            .expect("seed entity was just pushed")
            .tier = AnalysisTier::TierB;
        add_complete_repository_coverage(&mut document, 120, FactDomain::Entities, 1);
        add_complete_repository_coverage(&mut document, 121, FactDomain::Relations, 0);

        let plan = tests_select_plan(BTreeSet::from([symbol(11)]), Vec::new(), 20, false);
        let selection = run_tests_select(&document, &plan);

        assert!(selection.tests.is_empty());
        assert_eq!(selection.gaps.len(), 3);
        assert!(selection.gaps.iter().any(|gap| {
            gap.scope == symbol(11).to_string() && gap.reason == "no_related_test_observed"
        }));
        assert!(has_test_gap(&selection, "history_signal_unavailable"));
        assert!(has_test_gap(&selection, "runtime_coverage_unavailable"));
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
        assert!(has_test_gap(&unit_selection, "history_signal_unavailable"));
        assert!(has_test_gap(
            &unit_selection,
            "runtime_coverage_unavailable"
        ));

        let integration_plan = tests_select_plan(
            BTreeSet::from([symbol(11)]),
            vec![TestsSelectKind::Integration],
            20,
            false,
        );
        let integration_selection = run_tests_select(&document, &integration_plan);
        assert!(integration_selection.tests.is_empty());
        assert_eq!(integration_selection.gaps.len(), 3);
        assert!(
            integration_selection
                .gaps
                .iter()
                .any(|gap| gap.scope == symbol(11).to_string())
        );
    }

    #[test]
    fn tests_select_classifies_every_declared_test_kind() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/lib.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        let cases = [
            (TestsSelectKind::Unit, 21, 2, "src/unit_test.rs"),
            (
                TestsSelectKind::Integration,
                22,
                3,
                "tests/integration_case.rs",
            ),
            (TestsSelectKind::E2e, 23, 4, "tests/e2e_case.rs"),
            (TestsSelectKind::Contract, 24, 5, "tests/contract_case.rs"),
            (TestsSelectKind::Static, 25, 6, "src/static_lint.rs"),
            (TestsSelectKind::Build, 26, 7, "src/build_case.rs"),
        ];
        for (_, symbol_byte, file_byte, path) in cases {
            add_file(&mut document, file_byte, path);
            add_entity(&mut document, symbol_byte, file_byte, EntityKind::Test);
            add_calls(&mut document, symbol_byte + 100, symbol_byte, 11, 900);
        }

        for (kind, symbol_byte, _, _) in cases {
            let plan = tests_select_plan(BTreeSet::from([symbol(11)]), vec![kind], 20, false);
            let selection = run_tests_select(&document, &plan);
            assert_eq!(selection.tests.len(), 1);
            assert_eq!(selection.tests[0].test_id, symbol(symbol_byte));
            assert_eq!(selection.tests[0].kind, kind);
        }
    }

    #[test]
    fn tests_select_keeps_conventional_unit_tests_with_build_subjects() {
        let mut document = overview_document();
        add_file(&mut document, 1, "kdtree.py");
        add_file(&mut document, 2, "test_kdtree.py");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 21, 2, EntityKind::Test);
        let test = document
            .entities
            .last_mut()
            .expect("test entity was just pushed");
        test.display_name = "test_build_kdtree_recursion".to_owned();
        add_calls(&mut document, 110, 21, 11, 900);

        let unit_plan = tests_select_plan(
            BTreeSet::from([symbol(11)]),
            vec![TestsSelectKind::Unit],
            20,
            false,
        );
        let selection = run_tests_select(&document, &unit_plan);

        assert_eq!(selection.tests.len(), 1);
        assert_eq!(selection.tests[0].test_id, symbol(21));
        assert_eq!(selection.tests[0].kind, TestsSelectKind::Unit);
    }

    #[test]
    fn tests_select_uses_path_build_framework_history_and_execution_budget_signals() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/lib.rs");
        add_file(&mut document, 2, "tests/integration_python.py");
        add_file(&mut document, 3, "tests/integration_history.py");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::BuildTarget);
        let build_target = document
            .entities
            .last_mut()
            .expect("build target was just pushed");
        build_target.canonical_name = "rootlight-query".to_owned();
        build_target.display_name = "rootlight-query".to_owned();
        build_target.qualified_name = "rootlight-query".to_owned();
        add_entity(&mut document, 21, 2, EntityKind::Test);
        document
            .entities
            .last_mut()
            .expect("test entity was just pushed")
            .language = "python".to_owned();
        add_entity(&mut document, 22, 3, EntityKind::Test);
        document
            .entities
            .last_mut()
            .expect("history test entity was just pushed")
            .language = "python".to_owned();
        add_calls(&mut document, 110, 21, 11, 900);
        add_relation(
            &mut document,
            111,
            RelationEndpoint::Entity(symbol(21)),
            RelationPredicate::DependsOn,
            RelationEndpoint::Entity(symbol(12)),
            950,
        );
        add_relation(
            &mut document,
            112,
            RelationEndpoint::Entity(symbol(22)),
            RelationPredicate::CoChangedWith,
            RelationEndpoint::Entity(symbol(11)),
            800,
        );

        let mut plan = tests_select_plan(BTreeSet::new(), Vec::new(), 20, true);
        plan.seed_paths = vec!["src/lib.rs".to_owned()];
        plan.seed_build_targets = vec!["rootlight-query".to_owned()];
        plan.frameworks = vec!["pytest".to_owned()];
        plan.max_total_ms = Some(10_000);
        let selection = run_tests_select(&document, &plan);

        assert_eq!(selection.tests.len(), 2);
        assert_eq!(selection.tests[0].test_id, symbol(21));
        assert_eq!(selection.tests[0].kind, TestsSelectKind::Integration);
        assert_eq!(selection.tests[0].framework, "pytest");
        assert_eq!(selection.tests[0].estimated_cost_ms, Some(5_000));
        assert!(selection.coverage_strategy.build_target_signals);
        assert!(selection.coverage_strategy.history_signals);
        assert!(!has_test_gap(&selection, "history_signal_unavailable"));
        assert!(!has_test_gap(&selection, "seed_path_not_indexed"));
        assert!(!has_test_gap(&selection, "build_target_not_indexed"));
        assert!(!has_test_gap(&selection, "framework_not_observed"));

        plan.max_total_ms = Some(4_999);
        let constrained = run_tests_select(&document, &plan);
        assert!(constrained.tests.is_empty());
        assert!(has_test_gap(
            &constrained,
            "execution_budget_excluded_candidates"
        ));
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
            scope_paths: Vec::new(),
            scope_packages: Vec::new(),
            scope_services: Vec::new(),
            relation_policy: ChangeImpactRelationPolicy::Standard,
            max_depth,
            min_confidence,
            include_tests,
            include_history: false,
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
        assert_eq!(analysis.risk_summary.coverage, CoverageStatus::Bounded);
        assert!(analysis.tests.is_empty());
    }

    #[test]
    fn change_impact_rejects_unfunded_graph_workspace() {
        let document = overview_document();
        let mut plan = change_impact_plan(BTreeSet::from([symbol(11)]), Vec::new(), 3, 0, false, 1);
        plan.budget = QueryBudget::new().with_max_memory_bytes(1);
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);

        assert!(matches!(
            build_change_impact(
                &document,
                &plan,
                &control,
                &mut tracker,
                &mut limiting_resources,
            ),
            Err(QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit: 1,
            })
        ));
        assert_eq!(tracker.memory_bytes, 0);
        assert_eq!(tracker.rows, 0);
        assert_eq!(tracker.edges, 0);
    }

    #[test]
    fn change_impact_stops_fanout_after_cap_sentinel() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        for byte in 20_u8..28 {
            add_entity(&mut document, byte, 1, EntityKind::Function);
            add_calls(&mut document, byte.saturating_add(100), byte, 11, 900);
        }
        let plan = change_impact_plan(BTreeSet::from([symbol(11)]), Vec::new(), 3, 0, false, 1);
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);

        let analysis = build_change_impact(
            &document,
            &plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )
        .expect("bounded fanout returns the first dependent");

        assert_eq!(analysis.impacted[0].dependents.len(), 1);
        assert_eq!(analysis.impacted[0].dependents[0].symbol_id, symbol(20));
        assert_eq!(tracker.edges, 10);
        assert_eq!(limiting_resources, vec![QueryResource::Results]);
    }

    #[test]
    fn change_impact_truncation_keeps_distance_then_identity_order() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        for byte in [11, 12, 13, 14, 100] {
            add_entity(&mut document, byte, 1, EntityKind::Function);
        }
        add_calls(&mut document, 110, 12, 11, 900);
        add_calls(&mut document, 111, 13, 11, 900);
        add_calls(&mut document, 112, 100, 12, 900);
        add_calls(&mut document, 113, 14, 13, 900);
        let plan = change_impact_plan(BTreeSet::from([symbol(11)]), Vec::new(), 3, 0, false, 3);

        let analysis = run_change_impact(&document, &plan);
        let symbols: Vec<SymbolId> = analysis.impacted[0]
            .dependents
            .iter()
            .map(|entry| entry.symbol_id)
            .collect();

        assert_eq!(symbols, vec![symbol(12), symbol(13), symbol(14)]);
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
    fn change_impact_conservative_history_and_scope_change_observable_results() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/core.rs");
        add_file(&mut document, 2, "src/service/handler.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 2, EntityKind::Service);
        document
            .entities
            .last_mut()
            .expect("service entity was just pushed")
            .qualified_name = "rootlight-query::query-service::handler".to_owned();
        add_relation(
            &mut document,
            110,
            RelationEndpoint::Entity(symbol(12)),
            RelationPredicate::CallsRoute,
            RelationEndpoint::Entity(symbol(11)),
            900,
        );
        add_relation(
            &mut document,
            111,
            RelationEndpoint::Entity(symbol(12)),
            RelationPredicate::CoChangedWith,
            RelationEndpoint::Entity(symbol(11)),
            850,
        );

        let standard =
            change_impact_plan(BTreeSet::from([symbol(11)]), Vec::new(), 3, 0, false, 20);
        assert!(
            run_change_impact(&document, &standard).impacted[0]
                .dependents
                .is_empty()
        );

        let mut conservative = standard.clone();
        conservative.relation_policy = ChangeImpactRelationPolicy::Conservative;
        conservative.include_history = true;
        conservative.scope_paths = vec!["src/service".to_owned()];
        conservative.scope_packages = vec!["rootlight-query".to_owned()];
        conservative.scope_services = vec!["rootlight-query::query-service".to_owned()];
        let analysis = run_change_impact(&document, &conservative);
        assert_eq!(analysis.impacted[0].dependents.len(), 1);
        assert_eq!(analysis.impacted[0].dependents[0].symbol_id, symbol(12));
        assert!(
            analysis.impacted[0].dependents[0]
                .via
                .contains(&"calls_route".to_owned())
        );
        assert!(
            analysis
                .risk_summary
                .reasons
                .contains(&"bounded_history_signal_observed".to_owned())
        );

        conservative.scope_paths = vec!["src/other".to_owned()];
        assert!(
            run_change_impact(&document, &conservative).impacted[0]
                .dependents
                .is_empty()
        );
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
            target_paths: BTreeSet::new(),
            constraints: Vec::new(),
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
    fn plan_change_reports_internal_projection_caps_as_truncation() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        let mut targets = BTreeSet::new();
        for byte in 1..=65 {
            add_entity(&mut document, byte, 1, EntityKind::Function);
            targets.insert(symbol(byte));
        }

        let plan = plan_change_plan(PlanChangeObjective::Review, targets, BTreeSet::new(), 6);
        let (analysis, execution) = run_plan_change_with_execution(&document, &plan);

        assert_eq!(analysis.plan[0].targets.len(), PLAN_CHANGE_MAX_STEP_TARGETS);
        assert_eq!(
            analysis.context_pack_request.symbols.len(),
            PLAN_CHANGE_MAX_CONTEXT_ITEMS
        );
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
    fn plan_change_prioritizes_explicit_symbols_over_unrelated_entities() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/a.rs");
        for byte in 1..=40 {
            add_entity(&mut document, byte, 1, EntityKind::Function);
        }
        add_public_entity(&mut document, 200, 1, EntityKind::Function);
        add_entity(&mut document, 201, 1, EntityKind::Function);
        add_calls(&mut document, 210, 201, 200, 900);

        let mut plan = plan_change_plan(
            PlanChangeObjective::BugFix,
            BTreeSet::from([symbol(200)]),
            BTreeSet::new(),
            6,
        );
        plan.budget = plan.budget.with_max_rows(8);
        let (analysis, execution) = run_plan_change_with_execution(&document, &plan);

        assert_eq!(analysis.plan[0].targets, vec![symbol(200)]);
        assert_eq!(analysis.plan[2].targets, vec![symbol(201)]);
        assert_eq!(analysis.affected_scope.affected_symbols, 2);
        assert_eq!(analysis.affected_scope.affected_files, 1);
        assert!(analysis.affected_scope.touches_public_surface);
        assert!(execution.is_truncated());
        assert_eq!(execution.limiting_resources(), &[QueryResource::Rows]);
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
    fn plan_change_resolves_path_context_and_verifies_constraints() {
        let mut document = overview_document();
        add_file(&mut document, 1, "src/payments/api.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);

        let mut plan = plan_change_plan(
            PlanChangeObjective::Migration,
            BTreeSet::new(),
            BTreeSet::new(),
            8,
        );
        plan.target_paths.insert("src/payments".to_owned());
        plan.constraints = vec![
            "preserve the existing REST route".to_owned(),
            "avoid a schema change".to_owned(),
        ];
        let analysis = run_plan_change(&document, &plan);

        assert_eq!(analysis.plan[0].targets, vec![symbol(11)]);
        let constraint_step = analysis
            .plan
            .last()
            .expect("caller constraints produce a final verification step");
        assert!(
            constraint_step
                .action
                .contains("preserve the existing REST route")
        );
        assert!(constraint_step.action.contains("avoid a schema change"));
        assert_eq!(constraint_step.risks, vec!["constraint_violation"]);
        assert_eq!(
            constraint_step.verification.as_deref(),
            Some("verify every caller-provided constraint before completion")
        );
        assert_eq!(analysis.context_pack_request.symbols, vec![symbol(11)]);
        assert_eq!(analysis.context_pack_request.files, vec![file_id(1)]);
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
            scope: HistoryCompareScope::default(),
            change_kinds,
            include_unchanged_context: false,
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
    fn history_compare_rejects_unfunded_lineage_workspace() {
        let mut base = history_document(1);
        add_file(&mut base, 1, "src/a.rs");
        add_entity(&mut base, 11, 1, EntityKind::Function);
        let attacker_name = "attacker_controlled_name".repeat(4_096);
        let entity = base.entities.last_mut().expect("history entity exists");
        entity.canonical_name = attacker_name.clone();
        entity.display_name = attacker_name;
        let head = base.clone();
        let mut plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            0,
        );
        plan.budget = QueryBudget::new().with_max_memory_bytes(1);
        let mut tracker = UsageTracker::new(plan.budget);
        let mut limiting_resources = Vec::new();
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);

        assert!(matches!(
            build_history_compare(
                &base,
                &head,
                &plan,
                &control,
                &mut tracker,
                &mut limiting_resources,
            ),
            Err(QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit: 1,
            })
        ));
        assert_eq!(tracker.memory_bytes, 0);

        let required =
            history_compare_workspace_bytes(&base, &head).expect("workspace size is representable");
        plan.budget = QueryBudget::new().with_max_memory_bytes(required);
        let mut tracker = UsageTracker::new(plan.budget);
        let control = QueryControl::new(&cancellation, plan.budget.max_duration);
        build_history_compare(
            &base,
            &head,
            &plan,
            &control,
            &mut tracker,
            &mut limiting_resources,
        )
        .expect("the exact lineage workspace budget is sufficient");
        assert_eq!(tracker.memory_bytes, required);
    }

    #[test]
    fn history_compare_detects_added_removed_and_preserved_entities() {
        let mut base = history_document(1);
        add_file(&mut base, 1, "src/a.rs");
        add_entity(&mut base, 11, 1, EntityKind::Function);
        add_entity(&mut base, 12, 1, EntityKind::Function);

        let mut head = history_document(2);
        add_file(&mut head, 1, "src/a.rs");
        add_file(&mut head, 2, "src/b.rs");
        add_entity(&mut head, 11, 1, EntityKind::Function);
        add_entity(&mut head, 13, 2, EntityKind::Function);

        let mut plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        plan.include_unchanged_context = true;
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
        add_file_with_content(&mut head, 1, 2, "src/a.rs");
        add_history_entity(&mut head, 22, 1, 2, 0, 10, "sym_22", EntityKind::Function);

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
        add_file_with_content(&mut head, 1, 2, "src/a.rs");
        add_history_entity(&mut head, 31, 1, 2, 0, 10, "sym_31", EntityKind::Struct);

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
    fn history_compare_preserves_signature_lineage_when_the_symbol_id_changes() {
        let mut base = history_document(1);
        add_file_with_content(&mut base, 1, 1, "src/a.rs");
        add_history_entity(
            &mut base,
            32,
            1,
            1,
            5,
            15,
            "public_item",
            EntityKind::Function,
        );
        base.entities
            .last_mut()
            .expect("entity was just pushed")
            .visibility = EntityVisibility::Public;

        let mut head = history_document(2);
        add_file_with_content(&mut head, 1, 2, "src/a.rs");
        add_history_entity(
            &mut head,
            33,
            1,
            2,
            5,
            15,
            "public_item",
            EntityKind::Struct,
        );
        head.entities
            .last_mut()
            .expect("entity was just pushed")
            .visibility = EntityVisibility::Public;

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        let analysis = run_history_compare(&base, &head, &plan);

        assert_eq!(analysis.changes.len(), 1);
        assert_eq!(
            analysis.changes[0].kind,
            HistorySemanticChangeKind::SignatureModified
        );
        assert!(analysis.changes[0].breaking_candidate);
        assert_eq!(analysis.lineage.len(), 1);
        assert_eq!(analysis.lineage[0].base_symbol_id, symbol(32));
        assert_eq!(analysis.lineage[0].head_symbol_id, symbol(33));
        assert_eq!(analysis.lineage[0].confidence, 950);
        assert!(!analysis.lineage[0].is_rename);
        assert_eq!(analysis.breaking_candidates.len(), 1);
        assert_eq!(analysis.breaking_candidates[0].symbol_id, symbol(32));
    }

    #[test]
    fn history_compare_resolves_a_unique_exact_content_move() {
        let mut base = history_document(1);
        add_file_with_content(&mut base, 1, 9, "src/old.rs");
        add_history_entity(&mut base, 41, 1, 9, 5, 15, "run", EntityKind::Function);

        let mut head = history_document(2);
        add_file_with_content(&mut head, 2, 9, "src/new.rs");
        add_history_entity(&mut head, 42, 2, 9, 5, 15, "run", EntityKind::Function);

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        let analysis = run_history_compare(&base, &head, &plan);

        assert_eq!(analysis.changes.len(), 1);
        assert_eq!(analysis.changes[0].kind, HistorySemanticChangeKind::Moved);
        assert_eq!(analysis.changes[0].symbol_id, symbol(42));
        assert_eq!(analysis.changes[0].significance, 500);
        assert_eq!(analysis.lineage.len(), 1);
        assert_eq!(analysis.lineage[0].base_symbol_id, symbol(41));
        assert_eq!(analysis.lineage[0].head_symbol_id, symbol(42));
        assert_eq!(analysis.lineage[0].confidence, 1_000);
        assert!(!analysis.lineage[0].is_rename);
    }

    #[test]
    fn history_compare_resolves_a_unique_local_rename() {
        let mut base = history_document(1);
        add_file_with_content(&mut base, 1, 1, "src/a.rs");
        add_history_entity(&mut base, 51, 1, 1, 5, 13, "old_name", EntityKind::Function);

        let mut head = history_document(2);
        add_file_with_content(&mut head, 1, 2, "src/a.rs");
        add_history_entity(&mut head, 52, 1, 2, 5, 13, "new_name", EntityKind::Function);

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        let analysis = run_history_compare(&base, &head, &plan);

        assert_eq!(analysis.changes.len(), 1);
        assert_eq!(analysis.changes[0].kind, HistorySemanticChangeKind::Renamed);
        assert_eq!(analysis.changes[0].symbol_id, symbol(52));
        assert_eq!(analysis.changes[0].significance, 650);
        assert_eq!(analysis.lineage.len(), 1);
        assert_eq!(analysis.lineage[0].base_symbol_id, symbol(51));
        assert_eq!(analysis.lineage[0].head_symbol_id, symbol(52));
        assert_eq!(analysis.lineage[0].confidence, 900);
        assert!(analysis.lineage[0].is_rename);
    }

    #[test]
    fn history_compare_preserves_ambiguous_rename_candidates() {
        let mut base = history_document(1);
        add_file_with_content(&mut base, 1, 1, "src/a.rs");
        add_history_entity(&mut base, 61, 1, 1, 5, 13, "old_one", EntityKind::Function);
        add_history_entity(&mut base, 62, 1, 1, 5, 13, "old_two", EntityKind::Function);

        let mut head = history_document(2);
        add_file_with_content(&mut head, 1, 2, "src/a.rs");
        add_history_entity(&mut head, 63, 1, 2, 5, 13, "new_name", EntityKind::Function);

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        let analysis = run_history_compare(&base, &head, &plan);

        assert_eq!(analysis.changes.len(), 3);
        assert_eq!(
            analysis
                .changes
                .iter()
                .filter(|change| change.kind == HistorySemanticChangeKind::Removed)
                .count(),
            2
        );
        assert_eq!(
            analysis
                .changes
                .iter()
                .filter(|change| change.kind == HistorySemanticChangeKind::Added)
                .count(),
            1
        );
        assert!(analysis.lineage.is_empty());
    }

    #[test]
    fn history_compare_does_not_alias_an_unrelated_same_location_replacement() {
        let mut base = history_document(1);
        add_file_with_content(&mut base, 1, 1, "src/a.rs");
        add_history_entity(&mut base, 71, 1, 1, 5, 10, "alpha", EntityKind::Function);

        let mut head = history_document(2);
        add_file_with_content(&mut head, 1, 2, "src/a.rs");
        add_history_entity(&mut head, 72, 1, 2, 5, 9, "beta", EntityKind::Function);

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        let analysis = run_history_compare(&base, &head, &plan);

        assert_eq!(analysis.changes.len(), 2);
        assert!(
            analysis
                .changes
                .iter()
                .any(|change| change.kind == HistorySemanticChangeKind::Removed)
        );
        assert!(
            analysis
                .changes
                .iter()
                .any(|change| change.kind == HistorySemanticChangeKind::Added)
        );
        assert!(analysis.lineage.is_empty());
    }

    #[test]
    fn history_compare_reports_an_empty_complete_comparison_when_base_equals_head() {
        let mut document = history_document(1);
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);
        add_entity(&mut document, 12, 1, EntityKind::Function);

        let mut plan = history_compare_plan(
            history_generation(1),
            history_generation(1),
            BTreeSet::new(),
            100,
        );
        plan.include_unchanged_context = true;
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
    fn history_compare_excludes_unchanged_lineage_by_default() {
        let mut document = history_document(1);
        add_file(&mut document, 1, "src/a.rs");
        add_entity(&mut document, 11, 1, EntityKind::Function);

        let plan = history_compare_plan(
            history_generation(1),
            history_generation(1),
            BTreeSet::new(),
            100,
        );
        let analysis = run_history_compare(&document, &document, &plan);

        assert!(analysis.changes.is_empty());
        assert!(analysis.lineage.is_empty());
    }

    #[test]
    fn history_compare_reports_no_changes_for_equivalent_source_snapshots() {
        let mut base = history_document(1);
        add_file_with_content(&mut base, 1, 9, "src/a.rs");
        add_history_entity(&mut base, 81, 1, 9, 5, 15, "stable", EntityKind::Function);

        let mut head = history_document(2);
        add_file_with_content(&mut head, 1, 9, "src/a.rs");
        add_history_entity(&mut head, 82, 1, 9, 5, 25, "stable", EntityKind::Function);

        let mut plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        plan.include_unchanged_context = true;
        let analysis = run_history_compare(&base, &head, &plan);

        assert!(analysis.changes.is_empty());
        assert!(analysis.breaking_candidates.is_empty());
        assert_eq!(analysis.lineage.len(), 1);
        assert_eq!(analysis.lineage[0].base_symbol_id, symbol(81));
        assert_eq!(analysis.lineage[0].head_symbol_id, symbol(82));
        assert!(!analysis.lineage[0].is_rename);
    }

    #[test]
    fn history_compare_ignores_comment_only_span_and_synthetic_scope_churn() {
        let mut base = history_document(1);
        add_file_with_content(&mut base, 1, 1, "src/a.ts");
        add_history_entity(&mut base, 83, 1, 1, 0, 200, "src/a.ts", EntityKind::Module);
        add_history_entity(
            &mut base,
            84,
            1,
            1,
            10,
            100,
            "public_api",
            EntityKind::Function,
        );
        {
            let function = base.entities.last_mut().expect("entity was just pushed");
            function.container = Some(ContainerRef::Entity(symbol(83)));
            function.visibility = EntityVisibility::Public;
        }
        add_history_entity(
            &mut base,
            85,
            1,
            1,
            20,
            100,
            "scope@20:100",
            EntityKind::Namespace,
        );
        {
            let scope = base.entities.last_mut().expect("entity was just pushed");
            scope.display_name = "<lexical scope>".to_owned();
            scope.container = Some(ContainerRef::Entity(symbol(83)));
            scope.flags.push(EntityFlag::Synthetic);
        }

        let mut head = history_document(2);
        add_file_with_content(&mut head, 1, 2, "src/a.ts");
        add_history_entity(&mut head, 83, 1, 2, 0, 260, "src/a.ts", EntityKind::Module);
        add_history_entity(
            &mut head,
            84,
            1,
            2,
            10,
            160,
            "public_api",
            EntityKind::Function,
        );
        {
            let function = head.entities.last_mut().expect("entity was just pushed");
            function.container = Some(ContainerRef::Entity(symbol(83)));
            function.visibility = EntityVisibility::Public;
        }
        add_history_entity(
            &mut head,
            86,
            1,
            2,
            20,
            160,
            "scope@20:160",
            EntityKind::Namespace,
        );
        {
            let scope = head.entities.last_mut().expect("entity was just pushed");
            scope.display_name = "<lexical scope>".to_owned();
            scope.container = Some(ContainerRef::Entity(symbol(83)));
            scope.flags.push(EntityFlag::Synthetic);
        }

        let mut plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::new(),
            100,
        );
        plan.include_unchanged_context = true;
        let analysis = run_history_compare(&base, &head, &plan);

        assert!(analysis.changes.is_empty());
        assert!(analysis.breaking_candidates.is_empty());
        assert!(analysis.lineage.iter().any(|lineage| {
            lineage.base_symbol_id == symbol(85)
                && lineage.head_symbol_id == symbol(86)
                && !lineage.is_rename
        }));
    }

    #[test]
    fn history_compare_honors_the_change_kind_filter() {
        let mut base = history_document(1);
        add_file(&mut base, 1, "src/a.rs");
        add_entity(&mut base, 12, 1, EntityKind::Function);

        let mut head = history_document(2);
        add_file(&mut head, 2, "src/b.rs");
        add_entity(&mut head, 13, 2, EntityKind::Function);

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
    fn history_compare_applies_path_scope_to_both_generations() {
        let mut base = history_document(1);
        add_file_with_content(&mut base, 1, 1, "src/api.rs");
        add_file_with_content(&mut base, 2, 2, "vendor/other.rs");

        let mut head = history_document(2);
        add_file_with_content(&mut head, 1, 3, "src/api.rs");
        add_file_with_content(&mut head, 2, 4, "vendor/other.rs");
        add_history_entity(&mut head, 11, 1, 3, 0, 10, "in_scope", EntityKind::Function);
        add_history_entity(
            &mut head,
            12,
            2,
            4,
            0,
            10,
            "outside_scope",
            EntityKind::Function,
        );

        let mut plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::from([HistoryChangeKind::Entities]),
            100,
        );
        plan.scope.paths = vec!["src".to_owned()];
        let analysis = run_history_compare(&base, &head, &plan);

        assert_eq!(analysis.changes.len(), 1);
        assert_eq!(analysis.changes[0].symbol_id, symbol(11));
        assert_eq!(analysis.changes[0].kind, HistorySemanticChangeKind::Added);
    }

    #[test]
    fn history_compare_projects_relation_domains_split_and_merge() {
        let cases = [
            (
                RelationPredicate::OwnedBy,
                HistoryChangeKind::Ownership,
                HistorySemanticChangeKind::RelationChanged,
            ),
            (
                RelationPredicate::Tests,
                HistoryChangeKind::Tests,
                HistorySemanticChangeKind::RelationChanged,
            ),
            (
                RelationPredicate::CallsRoute,
                HistoryChangeKind::Routes,
                HistorySemanticChangeKind::ArchitectureChanged,
            ),
            (
                RelationPredicate::ReadsTable,
                HistoryChangeKind::Data,
                HistorySemanticChangeKind::RelationChanged,
            ),
            (
                RelationPredicate::LineageSplitFrom,
                HistoryChangeKind::Relations,
                HistorySemanticChangeKind::Split,
            ),
            (
                RelationPredicate::LineageMergedFrom,
                HistoryChangeKind::Relations,
                HistorySemanticChangeKind::Merged,
            ),
        ];
        for (index, (predicate, filter, expected)) in cases.into_iter().enumerate() {
            let mut base = history_document(1);
            add_file(&mut base, 1, "src/a.rs");
            add_entity(&mut base, 11, 1, EntityKind::Function);
            add_entity(&mut base, 12, 1, EntityKind::Function);

            let mut head = history_document(2);
            add_file(&mut head, 1, "src/a.rs");
            add_entity(&mut head, 11, 1, EntityKind::Function);
            add_entity(&mut head, 12, 1, EntityKind::Function);
            add_relation(
                &mut head,
                u8::try_from(index + 1).expect("fixture index fits"),
                RelationEndpoint::Entity(symbol(11)),
                predicate,
                RelationEndpoint::Entity(symbol(12)),
                900,
            );

            let plan = history_compare_plan(
                history_generation(1),
                history_generation(2),
                BTreeSet::from([filter]),
                100,
            );
            let analysis = run_history_compare(&base, &head, &plan);
            assert_eq!(
                analysis.changes.len(),
                1,
                "relation domain {predicate:?} emits one change"
            );
            assert_eq!(analysis.changes[0].kind, expected);
            assert_eq!(
                analysis.lineage.len(),
                1,
                "the changed stable identity is retained as lineage"
            );
        }
    }

    #[test]
    fn history_compare_reports_scoped_architecture_deltas() {
        let mut base = history_document(1);
        add_file(&mut base, 1, "src/services.rs");
        add_entity(&mut base, 10, 1, EntityKind::Service);
        add_entity(&mut base, 20, 1, EntityKind::Service);
        add_entity(&mut base, 11, 1, EntityKind::Function);
        add_entity(&mut base, 21, 1, EntityKind::Function);
        base.entities[2].container = Some(ContainerRef::Entity(symbol(10)));
        base.entities[3].container = Some(ContainerRef::Entity(symbol(20)));

        let mut head = history_document(2);
        add_file(&mut head, 1, "src/services.rs");
        add_entity(&mut head, 10, 1, EntityKind::Service);
        add_entity(&mut head, 20, 1, EntityKind::Service);
        add_entity(&mut head, 30, 1, EntityKind::Service);
        add_entity(&mut head, 11, 1, EntityKind::Function);
        add_entity(&mut head, 21, 1, EntityKind::Function);
        head.entities[3].container = Some(ContainerRef::Entity(symbol(10)));
        head.entities[4].container = Some(ContainerRef::Entity(symbol(20)));
        add_relation(
            &mut head,
            90,
            RelationEndpoint::Entity(symbol(11)),
            RelationPredicate::DependsOn,
            RelationEndpoint::Entity(symbol(21)),
            900,
        );

        let mut plan = history_compare_plan(
            history_generation(1),
            history_generation(2),
            BTreeSet::from([HistoryChangeKind::Architecture]),
            100,
        );
        plan.scope.paths = vec!["src".to_owned()];
        let analysis = run_history_compare(&base, &head, &plan);

        assert_eq!(analysis.architecture_delta.new_boundaries, 1);
        assert_eq!(analysis.architecture_delta.removed_boundaries, 0);
        assert_eq!(analysis.architecture_delta.new_cross_service_edges, 1);
        assert_eq!(analysis.architecture_delta.removed_cross_service_edges, 0);
        assert!(
            analysis
                .changes
                .iter()
                .any(|change| change.kind == HistorySemanticChangeKind::ArchitectureChanged)
        );
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

    fn advanced_file_paths(document: &NormalizedIrDocument) -> BTreeMap<FileId, &str> {
        document
            .files
            .iter()
            .map(|file| (file.id, file.path.as_str()))
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
                None,
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
            None,
            &control,
            &mut exact,
        )
        .expect("two matching entities fit the exact row budget");
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(exact.rows, 2);
    }

    #[test]
    fn advanced_scan_bounds_materialization_and_accounts_owned_rows() {
        let mut document = advanced_document();
        for entity in &mut document.entities {
            if entity.kind == EntityKind::Function {
                entity.canonical_name = "x".repeat(4_096);
            }
        }
        let file_paths = advanced_file_paths(&document);
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));

        let mut tiny = UsageTracker::new(QueryBudget::new().with_max_memory_bytes(1));
        let error = expect_advanced_error(
            eval_advanced_scan(
                &document,
                AdvancedEntityKind::Function,
                None,
                &file_paths,
                Some(1),
                &control,
                &mut tiny,
            ),
            "the borrowed match index exceeds a one-byte memory budget",
        );
        assert!(matches!(
            error,
            QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit: 1,
            }
        ));
        assert_eq!(tiny.memory_bytes, 0);

        let mut measured = UsageTracker::new(QueryBudget::new());
        let capped = eval_advanced_scan(
            &document,
            AdvancedEntityKind::Function,
            None,
            &file_paths,
            Some(1),
            &control,
            &mut measured,
        )
        .expect("one materialized row fits the default memory budget");
        assert_eq!(capped.rows.len(), 1);
        let capped_bytes = measured.memory_bytes;

        let mut exact = UsageTracker::new(QueryBudget::new().with_max_memory_bytes(capped_bytes));
        let capped = eval_advanced_scan(
            &document,
            AdvancedEntityKind::Function,
            None,
            &file_paths,
            Some(1),
            &control,
            &mut exact,
        )
        .expect("the measured memory budget admits the capped scan");
        assert_eq!(capped.rows.len(), 1);
        assert_eq!(exact.memory_bytes, capped_bytes);

        let mut uncapped =
            UsageTracker::new(QueryBudget::new().with_max_memory_bytes(capped_bytes));
        let error = expect_advanced_error(
            eval_advanced_scan(
                &document,
                AdvancedEntityKind::Function,
                None,
                &file_paths,
                None,
                &control,
                &mut uncapped,
            ),
            "the same budget cannot materialize the second owned row",
        );
        assert!(matches!(
            error,
            QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit,
            } if limit == capped_bytes
        ));
    }

    #[test]
    fn advanced_limit_pushes_its_cap_into_scan_materialization() {
        let document = advanced_document();
        let ast = AdvancedAstNode::Limit {
            input: Box::new(scan_functions()),
            max_rows: 1,
        };
        let file_paths = advanced_file_paths(&document);
        let cancellation = Cancellation::new();
        let control = QueryControl::new(&cancellation, Duration::from_secs(30));
        let mut tracker = UsageTracker::new(QueryBudget::new());

        let (set, truncated) = eval_advanced_node(
            &document,
            &ast,
            &file_paths,
            Some(2),
            &control,
            &mut tracker,
        )
        .expect("the limit admits one scan row");

        assert_eq!(set.rows.len(), 1);
        assert!(!truncated);
        assert_eq!(tracker.rows, 2);
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
                None,
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
            advanced_project_rows(
                advanced_integer_rows(&[1, 2]),
                &["id".to_owned()],
                &control,
                &mut UsageTracker::new(QueryBudget::new()),
            ),
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
            assert!(advanced_traversal_depths_within(&ast, ADVANCED_MAX_DEPTH));
        }
        for depth in [Some(0), Some(6)] {
            let ast = AdvancedAstNode::Traverse {
                seed: Some(symbol(11)),
                seed_from: None,
                relation: AdvancedRelationKind::Calls,
                direction: AdvancedTraverseDirection::Outbound,
                max_depth: depth,
            };
            assert!(!advanced_traversal_depths_within(&ast, ADVANCED_MAX_DEPTH));
        }
    }

    #[test]
    fn advanced_traversal_depth_respects_the_effective_plan_limit() {
        let ast = AdvancedAstNode::Traverse {
            seed: Some(symbol(11)),
            seed_from: None,
            relation: AdvancedRelationKind::Calls,
            direction: AdvancedTraverseDirection::Outbound,
            max_depth: Some(2),
        };
        let (_, plan_depth) = ast.derive_plan_shape();

        assert!(matches!(
            validate_advanced_depths(&ast, plan_depth, 1),
            Err(QueryError::PlanRejected {
                resource: QueryResource::Depth,
            })
        ));
        validate_advanced_depths(&ast, plan_depth, 2)
            .expect("the exact effective traversal depth is accepted");
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
