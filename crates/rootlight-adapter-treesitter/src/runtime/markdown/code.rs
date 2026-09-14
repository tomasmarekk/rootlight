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
        context: &mut NativeParseContext,
        cancellation: &Cancellation,
    ) -> Result<EmbeddedExtractionLimits, AdapterError> {
        let maximum_ranges = request
            .limits()
            .max_embedded_ranges()
            .min(self.config.max_included_ranges());
        let mut limited = EmbeddedExtractionLimits::default();
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
            let Some(ranges) = inline_ranges(body, request, maximum_ranges, cancellation)? else {
                candidates[index].syntax = "markdown.code_limit";
                limited.context = true;
                continue;
            };
            if remaining_nodes == 0 || context.remaining_progress_checks == 0 {
                candidates[index].syntax = "markdown.code_limit";
                limited.context = true;
                continue;
            }
            // Independent examples never coexist as parser inputs. Keep the
            // fragment cap for this tree and its nested replacements, while
            // syntax-node and native-work credit remain shared by the document.
            let mut used_ranges = ranges.len();
            let family = self
                .registry
                .family_for_language(&language)
                .ok_or_else(|| provider_failure("markdown-code-language"))?;
            let native_family = native_family_for_source(family, request.source().path().as_str());
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
            // The host already admitted UTF-8 and supplies the exact source
            // point; revalidating or rescanning its prefix per fence is redundant.
            context.range_origin = (body.start_byte(), body.start_position());
            let child_tree = match self.parse_native_tree(
                &child_request,
                &language_for(native_family),
                None,
                self.config.default_settings(),
                Some(&mut *context),
                cancellation,
            ) {
                Ok(tree) => tree,
                Err(AdapterError::ProviderFailed { code })
                    if matches!(
                        code.as_str(),
                        "syntax-node-parse-work-limit" | "syntax-depth-parse-work-limit"
                    ) =>
                {
                    cancellation.check()?;
                    context.remaining_progress_checks = 0;
                    candidates[index].syntax = "markdown.code_limit";
                    limited.context = true;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let child = inspect_tree(
                &child_tree,
                &child_request,
                remaining_nodes,
                limits.max_syntax_depth(),
                cancellation,
            )?;
            traversal.processed_nodes = traversal
                .processed_nodes
                .checked_add(child.processed_nodes)
                .ok_or_else(|| provider_failure("markdown-node-accounting"))?;
            traversal.max_depth = traversal.max_depth.max(child.max_depth);
            if let Some(diagnostic) = child.primary_diagnostic {
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
                candidates[index].syntax = if child.fully_traversed {
                    "markdown.code_error"
                } else {
                    "markdown.code_limit"
                };
                if !child.fully_traversed {
                    limited.context = true;
                    continue;
                }
            } else {
                candidates[index].syntax = "markdown.code_parsed";
            }
            let pack = self
                .query_packs
                .get(family)
                .ok_or_else(|| provider_failure("embedded-query-pack"))?;
            let mut captures = if let Some(maximum) = max_facts {
                let extraction = pack.extract(
                    family,
                    &child_tree,
                    request.source().bytes(),
                    remaining_nodes,
                    maximum.saturating_sub(candidates.len()),
                    cancellation,
                )?;
                limited.partition |= extraction.limit.is_some();
                extraction.candidates
            } else {
                pack.extract_identity(
                    family,
                    &child_tree,
                    request.source().bytes(),
                    remaining_nodes,
                    cancellation,
                )?
            };
            if family == GrammarFamily::Perl {
                limited.merge(self.extract_perl_replacements(
                    &child_tree,
                    &child_request,
                    request.limits(),
                    traversal,
                    &mut captures,
                    max_facts.map(|maximum| maximum.saturating_sub(candidates.len())),
                    &mut used_ranges,
                    Some(&mut *context),
                    cancellation,
                )?);
            }
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
                // Shared BASIC captures retain their native VBA labels. The
                // host cannot yet preserve the distinct family provenance.
                !matches!(
                    descriptor.language().as_str(),
                    "markdown" | "visual-basic-classic"
                ) && descriptor.language().as_str().as_bytes() == text
            })
            .map(|descriptor| descriptor.language().clone()));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::{
        AnalysisLimits, BatchThresholds, EncodingId, GenerationBoundSnapshot, StreamLimits,
    };
    use rootlight_ir::IrLimits;
    use rootlight_vfs::{RelativePath, RepositoryRoot};
    use std::{
        path::Path,
        time::{Duration, Instant},
    };

    #[test]
    fn exhausted_native_work_is_not_renewed_for_the_next_fence() {
        let source = format!(
            "~~~rust\nfn nested() {{ {}1{} }}\n~~~\n\n~~~rust\nfn later() {{}}\n~~~\n",
            "(".repeat(20_000),
            ")".repeat(20_000)
        );
        let temporary = tempfile::tempdir().unwrap();
        std::fs::write(temporary.path().join("examples.md"), &source).unwrap();
        let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
        let repository = RepositoryRoot::open(repository_id, temporary.path()).unwrap();
        let snapshot = repository
            .snapshot(
                &RelativePath::parse(Path::new("examples.md")).unwrap(),
                u64::try_from(source.len()).unwrap(),
            )
            .unwrap();
        let source_ref = SourceRef::new(
            repository_id,
            "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
                .parse()
                .unwrap(),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(source.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let stream = StreamLimits::new(
            64,
            4096,
            4 * 1024 * 1024,
            64,
            64 * 1024,
            128 * 1024,
            BatchThresholds::new(128, 256 * 1024, 8, 4096).unwrap(),
        )
        .unwrap();
        let limits = AnalysisLimits::new(
            source.len(),
            100_000,
            256,
            32,
            8 * 1024 * 1024,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap();
        let settings = ParserSettings::new(1024).unwrap();
        let provider = TreeSitterProvider::new(
            RuntimeConfig::new(source.len(), 100_000, 256, 32, 8, 1, 1024 * 1024, settings)
                .unwrap(),
        )
        .unwrap();
        let request = ParseRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
            LanguageId::new("markdown").unwrap(),
            EncodingId::new("utf-8").unwrap(),
            Vec::new(),
            &limits,
        )
        .unwrap();
        let cancellation = Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
        let host = provider
            .prepare_tree(&request, None, &[], settings, &cancellation)
            .unwrap();
        assert!(host.traversal.fully_traversed);
        for credit in [0, 1] {
            let mut traversal = inspect_tree(
                &host.tree,
                &request,
                limits.max_syntax_nodes(),
                limits.max_syntax_depth(),
                &cancellation,
            )
            .unwrap();
            let host_nodes = traversal.processed_nodes;
            let mut candidates = provider
                .query_packs
                .get(GrammarFamily::Markdown)
                .unwrap()
                .extract(
                    GrammarFamily::Markdown,
                    &host.tree,
                    source.as_bytes(),
                    limits.max_syntax_nodes(),
                    4096,
                    &cancellation,
                )
                .unwrap()
                .candidates;
            let mut context = NativeParseContext {
                expression_envelope: None,
                range_origin: (0, Point::new(0, 0)),
                remaining_progress_checks: credit,
            };
            let bounded = provider
                .extract_markdown_code(
                    &host.tree,
                    &request,
                    &mut traversal,
                    &mut candidates,
                    Some(4096),
                    &mut context,
                    &cancellation,
                )
                .unwrap();
            assert!(bounded.context);
            assert!(!bounded.partition);
            assert_eq!(context.remaining_progress_checks, 0);
            assert_eq!(traversal.processed_nodes, host_nodes);
            assert_eq!(
                candidates
                    .iter()
                    .filter(|candidate| candidate.syntax == "markdown.code_limit")
                    .count(),
                2
            );
            assert!(
                !candidates
                    .iter()
                    .any(|candidate| candidate.syntax.starts_with("rust."))
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
        }
    }
}
