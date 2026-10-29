//! One native Ruby program over source-owned ERB code ranges.
//! Host delimiters and literal content never enter the Ruby parser input.

use super::*;

fn mark(
    candidates: &mut [QueryCandidate],
    outcome: &'static str,
    cancel: &Cancellation,
) -> Result<(), AdapterError> {
    for (index, candidate) in candidates.iter_mut().enumerate() {
        if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            cancel.check()?;
        }
        candidate.syntax = match (candidate.syntax, outcome) {
            ("erb.statement_unavailable", "parsed") => "erb.statement_ruby_parsed",
            ("erb.output_unavailable", "parsed") => "erb.output_ruby_parsed",
            ("erb.statement_unavailable", "error") => "erb.statement_ruby_error",
            ("erb.output_unavailable", "error") => "erb.output_ruby_error",
            ("erb.statement_unavailable" | "erb.output_unavailable", "limit") => {
                "erb.embedded_limit"
            }
            (syntax, _) => syntax,
        };
    }
    Ok(())
}

impl TreeSitterProvider {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn extract_erb(
        &self,
        tree: &Tree,
        request: &ParseRequest<'_>,
        source: &NativeSourceView<'_>,
        traversal: &mut TraversalReport,
        candidates: &mut Vec<QueryCandidate>,
        max_facts: Option<usize>,
        cancellation: &Cancellation,
    ) -> Result<EmbeddedExtractionLimits, AdapterError> {
        let mut limited = EmbeddedExtractionLimits::default();
        if !request.included_ranges().is_empty() || !traversal.fully_traversed {
            mark(candidates, "limit", cancellation)?;
            limited.context = true;
            return Ok(limited);
        }
        if tree.root_node().has_error() {
            return Ok(limited);
        }
        let maximum = request
            .limits()
            .max_embedded_ranges()
            .min(self.config.max_included_ranges());
        let mut remaining = request
            .limits()
            .max_syntax_nodes()
            .saturating_sub(traversal.processed_nodes);
        let language =
            LanguageId::new("ruby").map_err(|_| provider_failure("erb-ruby-language"))?;
        let mut ranges = Vec::new();
        let mut logical_ranges = Vec::new();
        let mut cursor = tree.walk();
        'walk: loop {
            cancellation.check()?;
            let Some(next) = remaining.checked_sub(1) else {
                mark(candidates, "limit", cancellation)?;
                limited.context = true;
                return Ok(limited);
            };
            remaining = next;
            traversal.processed_nodes = traversal
                .processed_nodes
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            let node = cursor.node();
            if node.kind() == "code"
                && node
                    .parent()
                    .is_some_and(|parent| matches!(parent.kind(), "directive" | "output_directive"))
                && node.start_byte() < node.end_byte()
            {
                if ranges.len() >= maximum {
                    mark(candidates, "limit", cancellation)?;
                    limited.context = true;
                    return Ok(limited);
                }
                let body = source_ref_for_projected_span(
                    request.source().source_ref(),
                    source,
                    node.start_byte(),
                    node.end_byte(),
                )?;
                try_reserve_cancellable(&mut ranges, 1, cancellation, "erb-ranges-allocation")?;
                try_reserve_cancellable(
                    &mut logical_ranges,
                    1,
                    cancellation,
                    "erb-ranges-allocation",
                )?;
                logical_ranges.push(node.byte_range());
                ranges.push(IncludedRange::new(body.span(), language.clone()));
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    break 'walk;
                }
            }
        }
        // An empty included-range list means the entire source to Tree-sitter.
        // A literal/comment-only template must therefore create no Ruby child.
        if ranges.is_empty() {
            return Ok(limited);
        }
        if remaining == 0 {
            mark(candidates, "limit", cancellation)?;
            limited.context = true;
            return Ok(limited);
        }
        let limits = super::embedded::embedded_limits(request.limits(), remaining)?;
        let child_request = ParseRequest::new(
            request.source().clone(),
            language,
            request.encoding().clone(),
            ranges,
            &limits,
        )?;
        let child = self.prepare_tree(
            &child_request,
            None,
            &[],
            self.config.default_settings(),
            source,
            cancellation,
            None,
        )?;
        traversal.processed_nodes = traversal
            .processed_nodes
            .checked_add(child.traversal.processed_nodes)
            .ok_or(SinkError::AccountingOverflow)?;
        traversal.max_depth = traversal.max_depth.max(child.traversal.max_depth);
        if let Some(diagnostic) = child.traversal.primary_diagnostic {
            traversal.primary_diagnostic.get_or_insert(diagnostic);
            traversal.skipped_regions = traversal
                .skipped_regions
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            traversal.coverage = if matches!(diagnostic, PrimaryDiagnostic::ErrorRecovery { .. })
                || traversal.coverage == CoverageStatus::Unknown
            {
                CoverageStatus::Unknown
            } else {
                CoverageStatus::Bounded
            };
            if !child.traversal.fully_traversed {
                mark(candidates, "limit", cancellation)?;
                limited.context = true;
                return Ok(limited);
            }
            mark(candidates, "error", cancellation)?;
        } else {
            mark(candidates, "parsed", cancellation)?;
        }
        let pack = self
            .query_packs
            .get_for_native(GrammarFamily::Ruby, GrammarFamily::Ruby, cancellation)
            .map_err(|error| error.into_adapter_error("erb-ruby-query-pack"))?;
        let mut child_candidates = if let Some(maximum) = max_facts {
            let extraction = pack.extract(
                GrammarFamily::Ruby,
                &child.tree,
                source.as_bytes(),
                remaining,
                maximum.saturating_sub(candidates.len()),
                cancellation,
            )?;
            limited.partition |= extraction.limit.is_some();
            extraction.candidates
        } else {
            pack.extract_identity(
                GrammarFamily::Ruby,
                &child.tree,
                source.as_bytes(),
                remaining,
                cancellation,
            )?
        };
        child_candidates
            .retain(|child| !matches!(child.role, StructuralRole::Root | StructuralRole::Module));
        let first = logical_ranges
            .first()
            .ok_or_else(|| provider_failure("erb-ruby-ranges"))?
            .start;
        let last = logical_ranges
            .last()
            .ok_or_else(|| provider_failure("erb-ruby-ranges"))?
            .end;
        for child in &mut child_candidates {
            cancellation.check()?;
            if child.start < first || child.end > last || child.start > child.end {
                return Err(provider_failure("erb-ruby-candidate-span"));
            }
            if matches!(
                child.role,
                StructuralRole::Definition | StructuralRole::Reference | StructuralRole::CallName
            ) && !logical_ranges
                .partition_point(|range| range.start <= child.start)
                .checked_sub(1)
                .and_then(|index| logical_ranges.get(index))
                .is_some_and(|range| child.end <= range.end)
            {
                return Err(provider_failure("erb-ruby-endpoint-outside-code"));
            }
            child.native_depth = child
                .native_depth
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
        }
        try_reserve_cancellable(
            candidates,
            child_candidates.len(),
            cancellation,
            "erb-ruby-candidates-allocation",
        )?;
        candidates.extend(child_candidates);
        Ok(limited)
    }
}
