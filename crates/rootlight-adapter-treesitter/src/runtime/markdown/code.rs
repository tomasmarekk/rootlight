//! Source-bound language interpretation for fenced Markdown examples.
//! Each body uses a separate native tree under the host's shared budgets;
//! neither fences nor container markers become input to the child grammar.

use super::*;

impl TreeSitterProvider {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn extract_markdown_code(
        &self,
        tree: &Tree,
        request: &ParseRequest<'_>,
        traversal: &mut TraversalReport,
        candidates: &mut Vec<QueryCandidate>,
        max_facts: Option<usize>,
        mut used_ranges: usize,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let maximum_ranges = request
            .limits()
            .max_embedded_ranges()
            .min(self.config.max_included_ranges());
        let mut limited = false;
        for index in 0..candidates.len() {
            cancellation.check()?;
            let candidate = candidates[index];
            if candidate.syntax != "markdown.code_block"
                || candidate.role != StructuralRole::Signature
                || (!request.included_ranges().is_empty()
                    && !candidate_within_included_range(&candidate, request)?)
            {
                continue;
            }
            let block = tree
                .root_node()
                .named_descendant_for_byte_range(candidate.start, candidate.end)
                .filter(|node| {
                    node.kind() == "fenced_code_block"
                        && node.byte_range() == (candidate.start..candidate.end)
                })
                .ok_or_else(|| provider_failure("markdown-code-host-span"))?;
            let Some(language) = fence_language(
                block,
                request.source().bytes(),
                &self.registry,
                cancellation,
            )?
            else {
                continue;
            };
            let mut cursor = block.walk();
            let body = block
                .named_children(&mut cursor)
                .find(|node| node.kind() == "code_fence_content");
            let Some(body) = body else {
                candidates[index].syntax = "markdown.code_parsed";
                continue;
            };
            let remaining_nodes = request
                .limits()
                .max_syntax_nodes()
                .saturating_sub(traversal.processed_nodes);
            let Some(ranges) = inline_ranges(
                body,
                request,
                maximum_ranges.saturating_sub(used_ranges),
                cancellation,
            )?
            else {
                candidates[index].syntax = "markdown.code_limit";
                limited = true;
                continue;
            };
            if remaining_nodes == 0 {
                candidates[index].syntax = "markdown.code_limit";
                limited = true;
                continue;
            }
            used_ranges = used_ranges
                .checked_add(ranges.len())
                .ok_or_else(|| provider_failure("markdown-range-accounting"))?;
            let ranges = ranges
                .into_iter()
                .map(|range| IncludedRange::new(range.span(), language.clone()))
                .collect();
            let limits =
                super::super::embedded::embedded_limits(request.limits(), remaining_nodes)?;
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
                cancellation,
            )?;
            traversal.processed_nodes = traversal
                .processed_nodes
                .checked_add(child.traversal.processed_nodes)
                .ok_or_else(|| provider_failure("markdown-node-accounting"))?;
            traversal.max_depth = traversal.max_depth.max(child.traversal.max_depth);
            if let Some(diagnostic) = child.traversal.primary_diagnostic {
                traversal.primary_diagnostic.get_or_insert(diagnostic);
                traversal.skipped_regions = traversal
                    .skipped_regions
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("markdown-coverage-accounting"))?;
                traversal.coverage = match diagnostic {
                    PrimaryDiagnostic::ErrorRecovery { .. } => CoverageStatus::Unknown,
                    _ if traversal.coverage == CoverageStatus::Unknown => CoverageStatus::Unknown,
                    _ => CoverageStatus::Bounded,
                };
                candidates[index].syntax = if child.traversal.fully_traversed {
                    "markdown.code_error"
                } else {
                    "markdown.code_limit"
                };
                if !child.traversal.fully_traversed {
                    limited = true;
                    continue;
                }
            } else {
                candidates[index].syntax = "markdown.code_parsed";
            }
            let pack = self
                .query_packs
                .get(child.family)
                .ok_or_else(|| provider_failure("embedded-query-pack"))?;
            let mut captures = if let Some(maximum) = max_facts {
                let extraction = pack.extract(
                    child.family,
                    &child.tree,
                    request.source().bytes(),
                    remaining_nodes,
                    maximum.saturating_sub(candidates.len()),
                    cancellation,
                )?;
                limited |= extraction.limit.is_some();
                extraction.candidates
            } else {
                pack.extract_identity(
                    child.family,
                    &child.tree,
                    request.source().bytes(),
                    remaining_nodes,
                    cancellation,
                )?
            };
            captures.retain(|capture| capture.role != StructuralRole::Root);
            for capture in &mut captures {
                cancellation.check()?;
                if capture.role == StructuralRole::Module && capture.syntax.ends_with(".file") {
                    capture.start = body.start_byte();
                    capture.end = body.end_byte();
                }
                if capture.start < body.start_byte() || capture.end > body.end_byte() {
                    return Err(provider_failure("markdown-code-capture-span"));
                }
            }
            try_reserve_cancellable(
                candidates,
                captures.len(),
                cancellation,
                "markdown-code-captures-allocation",
            )?;
            candidates.extend(captures);
        }
        Ok(limited)
    }
}

fn fence_language(
    block: Node<'_>,
    source: &[u8],
    registry: &GrammarRegistry,
    cancellation: &Cancellation,
) -> Result<Option<LanguageId>, AdapterError> {
    if block.has_error() {
        return Ok(None);
    }
    let mut cursor = block.walk();
    for info in block.named_children(&mut cursor) {
        cancellation.check()?;
        if info.kind() != "info_string" {
            continue;
        }
        let Some(label) = info.named_child(0).filter(|node| node.kind() == "language") else {
            return Ok(None);
        };
        let text = source
            .get(label.byte_range())
            .ok_or_else(|| provider_failure("markdown-code-language-span"))?;
        let text = match text {
            b"js" => b"javascript".as_slice(),
            b"ts" => b"typescript",
            b"py" => b"python",
            b"rs" => b"rust",
            b"sh" => b"bash",
            b"c++" => b"cpp",
            b"c#" => b"csharp",
            b"yml" => b"yaml",
            b"ps1" => b"powershell",
            _ => text,
        };
        // Markdown examples are opaque text unless a different native language
        // is declared. Do not recursively interpret a document as executable code.
        return Ok(registry
            .descriptors()
            .iter()
            .find(|descriptor| {
                descriptor.language().as_str() != "markdown"
                    && descriptor.language().as_str().as_bytes() == text
            })
            .map(|descriptor| descriptor.language().clone()));
    }
    Ok(None)
}
