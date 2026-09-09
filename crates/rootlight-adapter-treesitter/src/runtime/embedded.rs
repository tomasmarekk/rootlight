//! Host-bound parsing of HTML/Astro script, style and server bodies.
//! Child grammars read original bytes through included ranges. Their candidates
//! join the host plan before identity closure, budgeting and parent assignment.

use rootlight_adapter_sdk::AnalysisLimits;

use super::*;

impl TreeSitterProvider {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn extract_embedded(
        &self,
        tree: &Tree,
        request: &ParseRequest<'_>,
        traversal: &mut TraversalReport,
        candidates: &mut Vec<QueryCandidate>,
        max_facts: Option<usize>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if request.language().as_str() == "markdown" {
            return self.extract_markdown(
                tree,
                request,
                traversal,
                candidates,
                max_facts,
                cancellation,
            );
        }
        let astro = request.language().as_str() == "astro";
        if !astro && request.language().as_str() != "html" {
            return Ok(false);
        }
        sort_cancellable_by(candidates, cancellation, |left, right| {
            (
                left.start,
                left.end,
                left.role,
                left.syntax,
                left.native_depth,
            )
                .cmp(&(
                    right.start,
                    right.end,
                    right.role,
                    right.syntax,
                    right.native_depth,
                ))
        })?;
        dedup_query_candidates(candidates, cancellation)?;
        let mut parsed_ranges = 0usize;
        let mut covered_expression_end = 0usize;
        let mut limited = false;
        let host_count = candidates.len();
        for index in 0..host_count {
            cancellation.check()?;
            let candidate = candidates[index];
            if !matches!(
                candidate.syntax,
                "html.embedded_text"
                    | "astro.embedded_text"
                    | "astro.frontmatter"
                    | "astro.expression"
            ) || (!request.included_ranges().is_empty()
                && !candidate_within_included_range(&candidate, request)?)
            {
                continue;
            }
            let expression = candidate.syntax == "astro.expression";
            if expression && candidate.start < covered_expression_end {
                if candidate.end > covered_expression_end {
                    return Err(provider_failure("expression-overlapping-ranges"));
                }
                candidates[index].syntax = "astro.expression_nested";
                continue;
            }
            let Some(body) = tree
                .root_node()
                .named_descendant_for_byte_range(candidate.start, candidate.end)
            else {
                return Err(provider_failure("embedded-host-node"));
            };
            if !(matches!(body.kind(), "raw_text" | "frontmatter_js_block")
                || expression
                    && matches!(
                        body.kind(),
                        "html_interpolation"
                            | "attribute_interpolation"
                            | "attribute_backtick_string"
                    ))
                || body.byte_range() != (candidate.start..candidate.end)
            {
                return Err(provider_failure("embedded-host-span"));
            }
            let language = if expression {
                let bytes = request
                    .source()
                    .bytes()
                    .get(candidate.start..candidate.end)
                    .ok_or_else(|| provider_failure("expression-source-range"))?;
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| provider_failure("expression-source-encoding"))?;
                // Spread attributes are host binding operations, not valid
                // standalone spread expressions; a TSX error would mislabel them.
                if body.kind() == "attribute_interpolation"
                    && text
                        .strip_prefix('{')
                        .is_some_and(|text| text.trim_start().starts_with("..."))
                {
                    continue;
                }
                Some("typescript")
            } else if astro {
                astro_body_language(body, request.source().bytes(), cancellation)?
            } else {
                body_language(body, request.source().bytes(), cancellation)?
            };
            let Some(language) = language else {
                continue;
            };
            let remaining_nodes = request
                .limits()
                .max_syntax_nodes()
                .saturating_sub(traversal.processed_nodes);
            if parsed_ranges
                >= request
                    .limits()
                    .max_embedded_ranges()
                    .min(self.config.max_included_ranges())
                || remaining_nodes == 0
            {
                candidates[index].syntax = if astro {
                    "astro.embedded_limit"
                } else {
                    "html.embedded_limit"
                };
                limited = true;
                continue;
            }
            parsed_ranges += 1;
            let limits = embedded_limits(request.limits(), remaining_nodes)?;
            let language_id =
                LanguageId::new(language).map_err(|_| provider_failure("embedded-language"))?;
            let source = source_ref_for_span(
                request.source().source_ref(),
                candidate.start,
                candidate.end,
            )
            .ok_or_else(|| provider_failure("embedded-source-span"))?;
            let child_request = ParseRequest::new(
                request.source().clone(),
                language_id.clone(),
                request.encoding().clone(),
                vec![IncludedRange::new(source.span(), language_id)],
                &limits,
            )?;
            let (family, native_family, child_tree, child_traversal) = if expression {
                let envelope = if body.kind() == "attribute_backtick_string" {
                    None
                } else {
                    let envelope = super::expression::ExpressionEnvelope::new(
                        request.source().bytes(),
                        candidate.start,
                        candidate.end,
                    )
                    .ok_or_else(|| provider_failure("expression-host-delimiters"))?;
                    if body.kind() == "html_interpolation"
                        && envelope
                            .contains_only_comments(request.source().bytes(), cancellation)?
                    {
                        None
                    } else {
                        Some(envelope)
                    }
                };
                let mut context = NativeParseContext {
                    expression_envelope: envelope,
                    range_origin: (body.start_byte(), body.start_position()),
                    remaining_progress_checks: ParseWorkLimit::from_syntax_limits(
                        remaining_nodes,
                        limits.max_syntax_depth(),
                    )
                    .max_progress_checks,
                };
                let tree = self.parse_native_tree(
                    &child_request,
                    &language_for(GrammarFamily::JavaScript),
                    None,
                    self.config.default_settings(),
                    Some(&mut context),
                    cancellation,
                )?;
                let report = inspect_tree(
                    &tree,
                    &child_request,
                    remaining_nodes,
                    limits.max_syntax_depth(),
                    cancellation,
                )?;
                covered_expression_end = candidate.end;
                (
                    GrammarFamily::TypeScript,
                    GrammarFamily::JavaScript,
                    tree,
                    report,
                )
            } else {
                let child = self.prepare_tree(
                    &child_request,
                    None,
                    &[],
                    self.config.default_settings(),
                    cancellation,
                )?;
                (
                    child.family,
                    native_family_for_source(child.family, request.source().path().as_str()),
                    child.tree,
                    child.traversal,
                )
            };
            traversal.processed_nodes = traversal
                .processed_nodes
                .checked_add(child_traversal.processed_nodes)
                .ok_or_else(|| provider_failure("embedded-node-accounting"))?;
            traversal.max_depth = traversal.max_depth.max(child_traversal.max_depth);
            if let Some(diagnostic) = child_traversal.primary_diagnostic {
                traversal.primary_diagnostic.get_or_insert(diagnostic);
                traversal.skipped_regions = traversal
                    .skipped_regions
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("embedded-coverage-accounting"))?;
                traversal.coverage = match diagnostic {
                    PrimaryDiagnostic::ErrorRecovery { .. } => CoverageStatus::Unknown,
                    _ if traversal.coverage == CoverageStatus::Unknown => CoverageStatus::Unknown,
                    _ => CoverageStatus::Bounded,
                };
                if !child_traversal.fully_traversed {
                    candidates[index].syntax = if astro {
                        "astro.embedded_limit"
                    } else {
                        "html.embedded_limit"
                    };
                    limited = true;
                    continue;
                }
                candidates[index].syntax = if astro {
                    "astro.embedded_parse_error"
                } else {
                    "html.embedded_parse_error"
                };
            } else {
                candidates[index].syntax = if astro {
                    "astro.embedded_parsed"
                } else {
                    match language {
                        "javascript" => "html.embedded_javascript",
                        "css" => "html.embedded_css",
                        _ => return Err(provider_failure("embedded-language")),
                    }
                };
            }
            let pack = self
                .query_packs
                .get_for_native(family, native_family)
                .ok_or_else(|| provider_failure("embedded-query-pack"))?;
            let mut child_candidates = if let Some(maximum) = max_facts {
                let extraction = pack.extract(
                    family,
                    &child_tree,
                    request.source().bytes(),
                    remaining_nodes,
                    maximum.saturating_sub(candidates.len()),
                    cancellation,
                )?;
                limited |= extraction.limit.is_some();
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
            // Server/client bodies own modules; template expressions do not.
            // Their native lexical scopes remain under the authored host owner.
            child_candidates.retain(|child| {
                child.role != StructuralRole::Root
                    && !(expression && child.role == StructuralRole::Module)
            });
            for child in &mut child_candidates {
                cancellation.check()?;
                if child.role == StructuralRole::Module {
                    child.start = candidate.start;
                    child.end = candidate.end;
                }
                if child.start < candidate.start || child.end > candidate.end {
                    return Err(provider_failure("embedded-candidate-span"));
                }
            }
            try_reserve_cancellable(
                candidates,
                child_candidates.len(),
                cancellation,
                "embedded-candidates-allocation",
            )?;
            candidates.extend(child_candidates);
        }
        Ok(limited)
    }
}

pub(super) fn embedded_limits(
    limits: &AnalysisLimits,
    nodes: usize,
) -> Result<AnalysisLimits, AdapterError> {
    AnalysisLimits::new(
        limits.max_source_bytes(),
        nodes,
        limits.max_syntax_depth(),
        limits.max_embedded_ranges(),
        limits.max_reported_memory_bytes(),
        limits.syntax_stream().clone(),
        limits.ir_stream().clone(),
        limits.ir().clone(),
    )
    .map_err(|_| provider_failure("embedded-limits"))
}

fn astro_body_language(
    body: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    if body.kind() == "frontmatter_js_block" {
        return Ok(Some("typescript"));
    }
    let Some(element) = body.parent().filter(|node| !node.has_error()) else {
        return Ok(None);
    };
    let Some(tag) = start_tag(element) else {
        return Ok(None);
    };
    let mut attributes = 0usize;
    let mut cursor = tag.walk();
    for attribute in tag.named_children(&mut cursor) {
        cancellation.check()?;
        if attribute.kind() == "tag_name" {
            continue;
        }
        // Spread/shorthand attributes can supply a runtime type or src. They
        // are not evidence that the body is an attribute-free processed script.
        if attribute.kind() != "attribute" {
            return Ok(None);
        }
        attributes = attributes.saturating_add(1);
        let Some(name) = attribute
            .named_child(0)
            .filter(|node| node.kind() == "attribute_name")
            .and_then(|node| source.get(node.byte_range()))
        else {
            return Ok(None);
        };
        // Preprocessor declarations and dynamic type attributes cannot be routed
        // as CSS/JavaScript without evaluating an Astro build configuration.
        if name.eq_ignore_ascii_case(b"lang") || name.eq_ignore_ascii_case(b"language") {
            return Ok(None);
        }
        if name.eq_ignore_ascii_case(b"type")
            && attribute.named_child(1).is_some_and(|node| {
                matches!(
                    node.kind(),
                    "attribute_interpolation" | "attribute_backtick_string"
                )
            })
        {
            return Ok(None);
        }
    }
    // Astro processes attribute-free client scripts as TypeScript. An attribute
    // opts out of processing; src bodies and non-script data retain explicit gaps.
    // https://docs.astro.build/en/guides/client-side-scripts/
    if element.kind() == "script_element" && attributes == 0 {
        return Ok(Some("typescript"));
    }
    body_language(body, source, cancellation)
}

fn body_language(
    body: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    let Some(element) = body.parent() else {
        return Ok(None);
    };
    if element.has_error() || !matches!(element.kind(), "script_element" | "style_element") {
        return Ok(None);
    }
    let mut ancestor = element.parent();
    while let Some(node) = ancestor {
        cancellation.check()?;
        if let Some(tag) = start_tag(node)
            && let Some(name) = tag
                .named_child(0)
                .and_then(|name| source.get(name.byte_range()))
            && [b"svg".as_slice(), b"math", b"noscript"]
                .iter()
                .any(|blocked| name.eq_ignore_ascii_case(blocked))
        {
            return Ok(None);
        }
        ancestor = node.parent();
    }
    let Some(tag) = start_tag(element) else {
        return Ok(None);
    };
    let mut kind = None;
    let mut legacy_language = None;
    let mut cursor = tag.walk();
    for attribute in tag
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "attribute")
    {
        cancellation.check()?;
        let Some(name) = attribute
            .named_child(0)
            .and_then(|node| source.get(node.byte_range()))
        else {
            return Ok(None);
        };
        if name.eq_ignore_ascii_case(b"src") && element.kind() == "script_element" {
            return Ok(None);
        }
        let destination = if name.eq_ignore_ascii_case(b"type") {
            &mut kind
        } else if name.eq_ignore_ascii_case(b"language") {
            &mut legacy_language
        } else {
            continue;
        };
        if destination.is_some() {
            return Ok(None);
        }
        let value = attribute
            .named_child(1)
            .and_then(|node| source.get(node.byte_range()))
            .unwrap_or_default();
        let value = unquote(value);
        // Encoded type values require HTML character-reference decoding. Never
        // guess a grammar from undecoded or conflicting routing attributes.
        if value.contains(&b'&') {
            return Ok(None);
        }
        *destination = Some(value);
    }
    if element.kind() == "style_element" {
        return Ok((kind
            .is_none_or(|kind| kind.is_empty() || kind.eq_ignore_ascii_case(b"text/css")))
        .then_some("css"));
    }
    if let Some(kind) = kind {
        let declared_empty = kind.is_empty();
        let kind = kind.trim_ascii();
        return Ok((declared_empty
            || kind.eq_ignore_ascii_case(b"module")
            || javascript_type(kind))
        .then_some("javascript"));
    }
    Ok(legacy_language
        .is_none_or(|language| {
            language.is_empty()
                || JAVASCRIPT_TYPES
                    .iter()
                    .filter_map(|kind| kind.strip_prefix("text/"))
                    .any(|known| language.eq_ignore_ascii_case(known.as_bytes()))
        })
        .then_some("javascript"))
}

fn start_tag(node: Node<'_>) -> Option<Node<'_>> {
    node.named_child(0)
        .filter(|child| child.kind() == "start_tag")
}

fn unquote(value: &[u8]) -> &[u8] {
    match (value.first(), value.last()) {
        (Some(b'\''), Some(b'\'')) | (Some(b'"'), Some(b'"')) if value.len() >= 2 => {
            value.get(1..value.len() - 1).unwrap_or_default()
        }
        _ => value,
    }
}

fn javascript_type(value: &[u8]) -> bool {
    // Script type uses an essence match, not a MIME parse: parameters do not
    // turn an author data block into executable JavaScript (HTML scripting).
    JAVASCRIPT_TYPES
        .iter()
        .any(|kind| value.eq_ignore_ascii_case(kind.as_bytes()))
}

const JAVASCRIPT_TYPES: &[&str] = &[
    "application/ecmascript",
    "application/javascript",
    "application/x-ecmascript",
    "application/x-javascript",
    "text/ecmascript",
    "text/javascript",
    "text/javascript1.0",
    "text/javascript1.1",
    "text/javascript1.2",
    "text/javascript1.3",
    "text/javascript1.4",
    "text/javascript1.5",
    "text/jscript",
    "text/livescript",
    "text/x-ecmascript",
    "text/x-javascript",
];
