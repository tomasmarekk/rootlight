//! Native Perl replacement expressions in unchanged host source coordinates.
//! Child trees share range, node, depth and native-work budgets; unknown evaluation
//! retains its storage barrier even when the first-stage expression is parsed.

use super::*;
use rootlight_adapter_sdk::AnalysisLimits;

struct ReplacementJob {
    tree: Tree,
    scopes: Vec<QueryCandidate>,
    depth: usize,
}

impl TreeSitterProvider {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn extract_perl_replacements(
        &self,
        tree: &Tree,
        request: &ParseRequest<'_>,
        budget: &AnalysisLimits,
        traversal: &mut TraversalReport,
        candidates: &mut Vec<QueryCandidate>,
        max_facts: Option<usize>,
        used_ranges: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let scopes = replacement_scopes(candidates, cancellation)?;
        if scopes.is_empty() {
            return Ok(false);
        }
        let mut pending = vec![ReplacementJob {
            tree: tree.clone(),
            scopes,
            depth: 0,
        }];
        let maximum_ranges = budget
            .max_embedded_ranges()
            .min(self.config.max_included_ranges());
        let mut limited = false;
        let mut context = NativeParseContext {
            expression_envelope: None,
            range_origin: (0, Point::new(0, 0)),
            remaining_progress_checks: ParseWorkLimit::from_syntax_limits(
                budget
                    .max_syntax_nodes()
                    .saturating_sub(traversal.processed_nodes),
                budget.max_syntax_depth(),
            )
            .max_progress_checks,
        };
        let pack = self
            .query_packs
            .get(GrammarFamily::Perl)
            .ok_or_else(|| provider_failure("perl-replacement-query-pack"))?;
        while let Some(job) = pending.pop() {
            for scope in job.scopes {
                cancellation.check()?;
                if !request.included_ranges().is_empty()
                    && !candidate_within_included_range(&scope, request)?
                {
                    continue;
                }
                let node = job
                    .tree
                    .root_node()
                    .named_descendant_for_byte_range(scope.start, scope.end)
                    .filter(|node| {
                        node.kind() == "substitution_regexp"
                            && node.byte_range() == (scope.start..scope.end)
                    })
                    .ok_or_else(|| provider_failure("perl-replacement-host-span"))?;
                if node.has_error() {
                    continue;
                }
                let Some(flags) = node
                    .child_by_field_name("modifiers")
                    .and_then(|flags| request.source().bytes().get(flags.byte_range()))
                else {
                    continue;
                };
                let single_eval = flags.iter().filter(|byte| **byte == b'e').count() == 1;
                let mut cursor = node.walk();
                let body = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "replacement");
                let Some(body) = body else {
                    if single_eval {
                        clear_storage_barrier(candidates, scope, cancellation)?;
                    }
                    continue;
                };
                let mut depth = job.depth;
                let mut parent = Some(body);
                while let Some(node) = parent {
                    cancellation.check()?;
                    depth = depth
                        .checked_add(1)
                        .ok_or_else(|| provider_failure("perl-replacement-depth"))?;
                    parent = node.parent();
                }
                let nodes = budget
                    .max_syntax_nodes()
                    .saturating_sub(traversal.processed_nodes);
                let depth_budget = budget.max_syntax_depth().saturating_sub(depth);
                if *used_ranges >= maximum_ranges
                    || nodes == 0
                    || depth_budget == 0
                    || context.remaining_progress_checks == 0
                {
                    limited = true;
                    continue;
                }
                *used_ranges += 1;
                let limits = embedded::embedded_limits(budget, nodes)?;
                let source = source_ref_for_span(
                    request.source().source_ref(),
                    body.start_byte(),
                    body.end_byte(),
                )
                .ok_or_else(|| provider_failure("perl-replacement-source-span"))?;
                let child_request = ParseRequest::new(
                    request.source().clone(),
                    request.language().clone(),
                    request.encoding().clone(),
                    vec![IncludedRange::new(
                        source.span(),
                        request.language().clone(),
                    )],
                    &limits,
                )?;
                context.range_origin = (body.start_byte(), body.start_position());
                let child_tree = self.parse_native_tree(
                    &child_request,
                    &language_for(GrammarFamily::Perl),
                    None,
                    self.config.default_settings(),
                    Some(&mut context),
                    cancellation,
                )?;
                let child = inspect_tree(
                    &child_tree,
                    &child_request,
                    nodes,
                    depth_budget,
                    cancellation,
                )?;
                traversal.processed_nodes = traversal
                    .processed_nodes
                    .checked_add(child.processed_nodes)
                    .ok_or_else(|| provider_failure("perl-replacement-node-accounting"))?;
                traversal.max_depth = traversal
                    .max_depth
                    .max(depth.saturating_add(child.max_depth));
                if let Some(diagnostic) = child.primary_diagnostic {
                    traversal.primary_diagnostic.get_or_insert(diagnostic);
                    traversal.skipped_regions = traversal
                        .skipped_regions
                        .checked_add(1)
                        .ok_or_else(|| provider_failure("perl-replacement-coverage-accounting"))?;
                    traversal.coverage = match diagnostic {
                        PrimaryDiagnostic::ErrorRecovery { .. } => CoverageStatus::Unknown,
                        _ if traversal.coverage == CoverageStatus::Unknown => {
                            CoverageStatus::Unknown
                        }
                        _ => CoverageStatus::Bounded,
                    };
                    limited |= !child.fully_traversed;
                    continue;
                }
                let mut captures = if let Some(maximum) = max_facts {
                    let extraction = pack.extract(
                        GrammarFamily::Perl,
                        &child_tree,
                        request.source().bytes(),
                        nodes,
                        maximum.saturating_sub(candidates.len()),
                        cancellation,
                    )?;
                    if extraction.limit.is_some() {
                        limited = true;
                        continue;
                    }
                    extraction.candidates
                } else {
                    pack.extract_identity(
                        GrammarFamily::Perl,
                        &child_tree,
                        request.source().bytes(),
                        nodes,
                        cancellation,
                    )?
                };
                // The replacement owns lexical scope, not another package universe.
                captures.retain(|capture| capture.syntax != "perl.file");
                for capture in &mut captures {
                    cancellation.check()?;
                    capture.native_depth = capture
                        .native_depth
                        .checked_add(depth)
                        .ok_or_else(|| provider_failure("perl-replacement-depth"))?;
                    if capture.start < body.start_byte() || capture.end > body.end_byte() {
                        return Err(provider_failure("perl-replacement-capture-span"));
                    }
                    if capture.syntax == "perl.assignment" {
                        capture.syntax = "perl.non_linear_assignment";
                    }
                }
                let nested = replacement_scopes(&captures, cancellation)?;
                if !nested.is_empty() {
                    try_reserve_cancellable(
                        &mut pending,
                        1,
                        cancellation,
                        "perl-replacement-jobs",
                    )?;
                    pending.push(ReplacementJob {
                        tree: child_tree,
                        scopes: nested,
                        depth,
                    });
                }
                // Host string interpolation is not a second interpretation of /e.
                let mut retained = 0usize;
                for index in 0..candidates.len() {
                    cancellation.check()?;
                    let candidate = candidates[index];
                    if candidate.start < body.start_byte() || candidate.end > body.end_byte() {
                        candidates[retained] = candidate;
                        retained += 1;
                    }
                }
                candidates.truncate(retained);
                try_reserve_cancellable(
                    candidates,
                    captures.len(),
                    cancellation,
                    "perl-replacement-candidates",
                )?;
                candidates.extend(captures);
                if single_eval {
                    clear_storage_barrier(candidates, scope, cancellation)?;
                }
            }
        }
        Ok(limited)
    }
}

fn replacement_scopes(
    candidates: &[QueryCandidate],
    cancellation: &Cancellation,
) -> Result<Vec<QueryCandidate>, AdapterError> {
    let mut scopes = Vec::new();
    for &candidate in candidates {
        cancellation.check()?;
        if candidate.syntax == "perl.substitution" && candidate.role == StructuralRole::Scope {
            try_reserve_cancellable(&mut scopes, 1, cancellation, "perl-replacement-scopes")?;
            scopes.push(candidate);
        }
    }
    sort_cancellable_by(&mut scopes, cancellation, |a, b| {
        (a.start, a.end).cmp(&(b.start, b.end))
    })?;
    dedup_query_candidates(&mut scopes, cancellation)?;
    Ok(scopes)
}

fn clear_storage_barrier(
    candidates: &mut [QueryCandidate],
    scope: QueryCandidate,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    for candidate in candidates {
        cancellation.check()?;
        if candidate.start == scope.start
            && candidate.end == scope.end
            && candidate.syntax == "perl.code_flow_barrier"
            && candidate.role == StructuralRole::Expression
        {
            candidate.syntax = "perl.parsed_substitution";
        }
    }
    Ok(())
}
