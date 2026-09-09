//! Native inline Markdown parsing in original document coordinates.
//! Each block is interpreted independently so unmatched delimiters cannot join
//! unrelated paragraphs; block continuation markers are excluded, not rewritten.

use super::*;

impl TreeSitterProvider {
    pub(super) fn extract_markdown_inline(
        &self,
        tree: &Tree,
        request: &ParseRequest<'_>,
        traversal: &mut TraversalReport,
        candidates: &mut Vec<QueryCandidate>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        sort_cancellable_by(candidates, cancellation, |left, right| {
            left.retention_rank().cmp(&right.retention_rank())
        })?;
        dedup_query_candidates(candidates, cancellation)?;
        let host_count = candidates.len();
        let maximum_ranges = request
            .limits()
            .max_embedded_ranges()
            .min(self.config.max_included_ranges());
        let mut used_ranges = 0usize;
        let mut limited = false;
        for index in 0..host_count {
            cancellation.check()?;
            let candidate = candidates[index];
            if candidate.syntax != "markdown.inline"
                || candidate.role != StructuralRole::Signature
                || (!request.included_ranges().is_empty()
                    && !candidate_within_included_range(&candidate, request)?)
            {
                continue;
            }
            let remaining_nodes = request
                .limits()
                .max_syntax_nodes()
                .saturating_sub(traversal.processed_nodes);
            let body = tree
                .root_node()
                .named_descendant_for_byte_range(candidate.start, candidate.end)
                .filter(|node| {
                    node.kind() == "inline" && node.byte_range() == (candidate.start..candidate.end)
                })
                .ok_or_else(|| provider_failure("markdown-inline-host-span"))?;
            let Some(ranges) = inline_ranges(
                body,
                request,
                maximum_ranges.saturating_sub(used_ranges),
                cancellation,
            )?
            else {
                candidates[index].syntax = "markdown.inline_limit";
                limited = true;
                continue;
            };
            if remaining_nodes == 0 {
                candidates[index].syntax = "markdown.inline_limit";
                limited = true;
                continue;
            }
            used_ranges = used_ranges
                .checked_add(ranges.len())
                .ok_or_else(|| provider_failure("markdown-range-accounting"))?;
            let limits = super::embedded::embedded_limits(request.limits(), remaining_nodes)?;
            let child_request = ParseRequest::new(
                request.source().clone(),
                request.language().clone(),
                request.encoding().clone(),
                ranges,
                &limits,
            )?;
            let child = self.parse_native_tree(
                &child_request,
                &tree_sitter_md::INLINE_LANGUAGE.into(),
                None,
                self.config.default_settings(),
                cancellation,
            )?;
            let report = inspect_tree(
                &child,
                &child_request,
                remaining_nodes,
                limits.max_syntax_depth(),
                cancellation,
            )?;
            traversal.processed_nodes = traversal
                .processed_nodes
                .checked_add(report.processed_nodes)
                .ok_or_else(|| provider_failure("markdown-node-accounting"))?;
            traversal.max_depth = traversal.max_depth.max(report.max_depth);
            if let Some(diagnostic) = report.primary_diagnostic {
                traversal.coverage = match diagnostic {
                    PrimaryDiagnostic::ErrorRecovery { .. } => CoverageStatus::Unknown,
                    _ if traversal.coverage == CoverageStatus::Unknown => CoverageStatus::Unknown,
                    _ => CoverageStatus::Bounded,
                };
                traversal.primary_diagnostic.get_or_insert(diagnostic);
                traversal.skipped_regions = traversal
                    .skipped_regions
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("markdown-coverage-accounting"))?;
                candidates[index].syntax = if report.fully_traversed {
                    "markdown.inline_error"
                } else {
                    "markdown.inline_limit"
                };
                limited |= !report.fully_traversed;
                continue;
            }
            candidates[index].syntax = "markdown.inline_parsed";
            capture_inline(&child, request.source().bytes(), candidates, cancellation)?;
        }
        Ok(limited)
    }
}

fn inline_ranges(
    body: Node<'_>,
    request: &ParseRequest<'_>,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<Option<Vec<IncludedRange>>, AdapterError> {
    let mut ranges = Vec::new();
    let mut start = body.start_byte();
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() != "block_continuation" {
            return Err(provider_failure("markdown-inline-exclusion-kind"));
        }
        if !append_range(
            &mut ranges,
            start,
            child.start_byte(),
            request,
            maximum,
            cancellation,
        )? {
            return Ok(None);
        }
        start = child.end_byte();
    }
    if !append_range(
        &mut ranges,
        start,
        body.end_byte(),
        request,
        maximum,
        cancellation,
    )? || ranges.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(ranges))
}

fn append_range(
    ranges: &mut Vec<IncludedRange>,
    start: usize,
    end: usize,
    request: &ParseRequest<'_>,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if start >= end {
        return Ok(true);
    }
    if ranges.len() >= maximum {
        return Ok(false);
    }
    let source = source_ref_for_span(request.source().source_ref(), start, end)
        .ok_or_else(|| provider_failure("markdown-inline-source-span"))?;
    try_reserve_cancellable(ranges, 1, cancellation, "markdown-ranges-allocation")?;
    ranges.push(IncludedRange::new(
        source.span(),
        request.language().clone(),
    ));
    Ok(true)
}

fn capture_inline(
    tree: &Tree,
    source: &[u8],
    candidates: &mut Vec<QueryCandidate>,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut cursor = tree.walk();
    loop {
        cancellation.check()?;
        let node = cursor.node();
        let parent_kind = node.parent().map(|parent| parent.kind());
        let capture = match node.kind() {
            "link_label" if matches!(parent_kind, Some("full_reference_link" | "image")) => {
                Some((StructuralRole::Reference, "markdown.reference_label", false))
            }
            "shortcut_link" => Some((StructuralRole::Reference, "markdown.shortcut_link", false)),
            "collapsed_reference_link" => {
                Some((StructuralRole::Reference, "markdown.collapsed_link", false))
            }
            "image" => image_reference_kind(node, source, cancellation)?
                .map(|syntax| (StructuralRole::Reference, syntax, false)),
            "link_destination" | "uri_autolink" | "email_autolink" => Some((
                StructuralRole::Reference,
                "markdown.link_destination",
                false,
            )),
            "html_tag" | "latex_block" => {
                Some((StructuralRole::Signature, "markdown.inline_embedded", true))
            }
            "code_span" => Some((StructuralRole::StringLiteral, "markdown.code_span", false)),
            _ => None,
        };
        if let Some((role, syntax, required)) = capture {
            if candidates.len() >= crate::query_pack::HARD_MAX_QUERY_FACTS {
                return Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::Records,
                    observed: candidates.len().saturating_add(1),
                    limit: crate::query_pack::HARD_MAX_QUERY_FACTS,
                }));
            }
            try_reserve_cancellable(candidates, 1, cancellation, "markdown-captures-allocation")?;
            candidates.push(QueryCandidate {
                start: node.start_byte(),
                end: node.end_byte(),
                role,
                syntax,
                required,
                native_depth: 0,
            });
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(());
            }
        }
    }
}

fn image_reference_kind(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        // Full references have a separate label capture. Parentheses also
        // distinguish inline images with an empty destination from shortcuts.
        if matches!(
            child.kind(),
            "link_label" | "link_destination" | "link_title" | "("
        ) {
            return Ok(None);
        }
    }
    let text = source
        .get(node.byte_range())
        .ok_or_else(|| provider_failure("markdown-image-source"))?;
    Ok(Some(if text.ends_with(b"[]") {
        "markdown.collapsed_image"
    } else {
        "markdown.shortcut_image"
    }))
}
