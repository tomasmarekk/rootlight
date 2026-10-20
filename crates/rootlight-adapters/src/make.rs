//! First-party structural parser for static Makefile declarations.
//! It preserves authored byte spans for variables, targets, and static prerequisites.
//! Literal recipe variable uses retain their target owner and exact name span.
//! Expansion, shell execution, directives, and continued headers remain bounded gaps.

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, CoverageReport, DescriptorError, DiagnosticCode,
    DomainCoverage, EncodingId, LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider,
    ParseReport, ParseRequest, ResourceUsage, SinkError, StreamEnd, StreamUsage, SyntaxFact,
    SyntaxFactBatch, SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId, content_hash};
use rootlight_ir::{
    AnalysisTier, CoverageStatus, DiagnosticSeverity, FactDomain, SourceRef, SourceSpan,
};

const CHECK_INTERVAL: usize = 4096;
const MAX_SCOPED_GAPS: usize = 32;
const MAX_GAP_CODE_BYTES: usize = 40;
const MAX_GAP_IR_BYTES: usize = 128;
// The syntax sink accounts one logical record plus its code; this larger
// reservation keeps optional gap evidence inside caller-owned stream quotas.
const MAX_GAP_ACCOUNTED_BYTES: usize = 256;

/// Bounded native provider for source-backed Makefile definitions.
pub struct MakeParseProvider {
    capabilities: ParseCapabilities,
}

impl MakeParseProvider {
    /// Returns the content identity of this first-party frontend.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("make.rs"))
    }

    /// Creates the parser under the caller's source and syntax-node ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError`] when a ceiling is zero.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed `make` language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        let language = LanguageId::new("make").expect("fixed Make language label");
        let capabilities = ParseCapabilities::new(
            vec![language],
            vec![EncodingId::utf8()],
            max_source_bytes,
            max_syntax_nodes,
            128,
            0,
            false,
            false,
            true,
            1,
            MemoryEnforcement::Unavailable,
        )?;
        Ok(Self { capabilities })
    }

    /// Counts the same syntax facts that parsing will emit before admission.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for invalid UTF-8, cancellation, or a syntax ceiling.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        let parsed = scan_source(
            request.source().bytes(),
            request
                .limits()
                .max_syntax_nodes()
                .min(self.capabilities.max_syntax_nodes()),
            cancellation,
        )?;
        Ok(syntax_fact_count(&parsed))
    }
}

impl ParseProvider for MakeParseProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let source = request.source();
        let parsed = scan_source(
            source.bytes(),
            request
                .limits()
                .max_syntax_nodes()
                .min(self.capabilities.max_syntax_nodes()),
            cancellation,
        )?;
        let fact_count = syntax_fact_count(&parsed);
        let file = source.source_ref().span().file();
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(fact_count)
            .map_err(|_| SinkError::AllocationFailed)?;
        facts.push(SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.source_ref().span(),
            0,
            label("make.source_file"),
        ));
        let mut next_id = 2_u64;
        for declaration in &parsed.declarations {
            cancellation.check()?;
            let declaration_id = next_id;
            next_id = next_id
                .checked_add(2)
                .ok_or_else(|| failure("make-syntax-node-limit"))?;
            let kind = match declaration.kind {
                DeclarationKind::Variable => "make.variable.declaration",
                DeclarationKind::Target => "make.target.declaration",
            };
            facts.push(SyntaxFact::new(
                declaration_id,
                Some(1),
                SyntaxFactKind::Declaration,
                source_span(file, declaration.name_start, declaration.line_end)?,
                1,
                label(kind),
            ));
            facts.push(SyntaxFact::new(
                declaration_id + 1,
                Some(declaration_id),
                SyntaxFactKind::Occurrence,
                source_span(file, declaration.name_start, declaration.name_end)?,
                2,
                label("make.name.definition"),
            ));
            for reference in &declaration.prerequisites {
                facts.push(SyntaxFact::new(
                    next_id,
                    Some(declaration_id),
                    SyntaxFactKind::Occurrence,
                    source_span(file, reference.start, reference.end)?,
                    2,
                    label("make.target.reference"),
                ));
                next_id = next_id
                    .checked_add(1)
                    .ok_or_else(|| failure("make-syntax-node-limit"))?;
            }
            for reference in &declaration.recipe_variables {
                facts.push(SyntaxFact::new(
                    next_id,
                    Some(declaration_id),
                    SyntaxFactKind::Occurrence,
                    source_span(file, reference.start, reference.end)?,
                    2,
                    label("make.variable.reference"),
                ));
                next_id = next_id
                    .checked_add(1)
                    .ok_or_else(|| failure("make-syntax-node-limit"))?;
            }
        }
        for batch in facts.chunks(32) {
            cancellation.check()?;
            let mut emitted = Vec::new();
            emitted
                .try_reserve_exact(batch.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            emitted.extend_from_slice(batch);
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), emitted, Vec::new()),
                cancellation,
            )?;
        }
        let diagnostic_budget = gap_diagnostic_budget(request, sink.staged_usage());
        if parsed.skipped_regions > 0 && diagnostic_budget > 0 {
            let mut diagnostics = Vec::new();
            let diagnostic_count = parsed.skipped_regions.min(diagnostic_budget);
            diagnostics
                .try_reserve_exact(diagnostic_count)
                .map_err(|_| SinkError::AllocationFailed)?;
            let precise_count = if parsed.skipped_regions > diagnostic_budget {
                diagnostic_count - 1
            } else {
                diagnostic_count
            };
            for gap in parsed.gaps.iter().take(precise_count) {
                diagnostics.push(gap_diagnostic(
                    source.source_ref(),
                    gap.start,
                    gap.end,
                    gap.kind.code(),
                )?);
            }
            if parsed.skipped_regions > diagnostic_budget {
                let start = parsed
                    .gaps
                    .get(precise_count)
                    .ok_or_else(|| failure("make-gap-accounting"))?
                    .start;
                diagnostics.push(gap_diagnostic(
                    source.source_ref(),
                    start,
                    parsed.last_gap_end,
                    "make-gap-diagnostic-limit",
                )?);
            }
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), Vec::new(), diagnostics),
                cancellation,
            )?;
        }
        let usage = sink.staged_usage();
        let source_bytes = source.bytes().len();
        let reference_count: usize = parsed
            .declarations
            .iter()
            .map(|declaration| declaration.prerequisites.len() + declaration.recipe_variables.len())
            .sum();
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Bounded,
            source_bytes,
            source_bytes,
            parsed.skipped_regions,
            vec![
                DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                DomainCoverage::new(
                    FactDomain::Entities,
                    CoverageStatus::Bounded,
                    parsed.declarations.len(),
                    parsed.declarations.len(),
                    0,
                )?,
                DomainCoverage::new(
                    FactDomain::Relations,
                    CoverageStatus::Bounded,
                    reference_count,
                    reference_count,
                    0,
                )?,
            ],
        )?;
        Ok(ParseReport::new(
            coverage,
            ResourceUsage::new(source_bytes, facts.len(), facts.len(), 2, None, usage),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

fn gap_diagnostic_budget(request: &ParseRequest<'_>, used: StreamUsage) -> usize {
    let syntax = request.limits().syntax_stream();
    if used.batches() >= syntax.max_batches() {
        return 0;
    }
    let batch = syntax.batch();
    let ir = request.limits().ir();
    [
        MAX_SCOPED_GAPS,
        batch.max_diagnostics(),
        batch.max_diagnostic_bytes() / MAX_GAP_CODE_BYTES,
        batch.max_output_bytes() / MAX_GAP_ACCOUNTED_BYTES,
        syntax.max_diagnostics().saturating_sub(used.diagnostics()),
        syntax
            .max_diagnostic_bytes()
            .saturating_sub(used.diagnostic_bytes())
            / MAX_GAP_CODE_BYTES,
        syntax
            .max_string_bytes()
            .saturating_sub(used.string_bytes())
            / MAX_GAP_CODE_BYTES,
        syntax
            .max_output_bytes()
            .saturating_sub(used.output_bytes())
            / MAX_GAP_ACCOUNTED_BYTES,
        ir.max_diagnostics,
        ir.max_skipped_regions,
        ir.max_total_diagnostic_bytes / MAX_GAP_IR_BYTES,
    ]
    .into_iter()
    .min()
    .unwrap_or(0)
}

fn gap_diagnostic(
    full: &SourceRef,
    start: usize,
    end: usize,
    code: &str,
) -> Result<AdapterDiagnostic, AdapterError> {
    debug_assert!(code.len() <= MAX_GAP_CODE_BYTES);
    let span = source_span(full.span().file(), start, end)?;
    let source = SourceRef::new(
        full.repository(),
        full.generation(),
        span,
        full.content_hash(),
        None,
    );
    Ok(AdapterDiagnostic::new(
        DiagnosticCode::new(code).map_err(|_| failure("make-diagnostic-code"))?,
        DiagnosticSeverity::Warning,
        Some(source),
        CoverageStatus::Bounded,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclarationKind {
    Target,
    Variable,
}

struct Reference {
    start: usize,
    end: usize,
}

struct Declaration {
    kind: DeclarationKind,
    name_start: usize,
    name_end: usize,
    line_end: usize,
    prerequisites: Vec<Reference>,
    recipe_variables: Vec<Reference>,
}

struct ParsedSource {
    declarations: Vec<Declaration>,
    skipped_regions: usize,
    fact_count: usize,
    gaps: Vec<Gap>,
    last_gap_end: usize,
    recipe_owner: Option<usize>,
}

#[derive(Clone, Copy)]
enum GapKind {
    ContinuedHeader,
    DefineBlock,
    Recipe,
    UnsupportedStatement,
    UnsupportedTarget,
    UnsupportedPrerequisite,
    OrderOnlyPrerequisite,
    VariableValue,
    DynamicTarget,
    DynamicPrerequisite,
    MultipleTargetPrerequisite,
}

impl GapKind {
    const fn code(self) -> &'static str {
        match self {
            Self::ContinuedHeader => "make-continued-header-not-analyzed",
            Self::DefineBlock => "make-define-block-not-analyzed",
            Self::Recipe => "make-recipe-not-analyzed",
            Self::UnsupportedStatement => "make-statement-not-analyzed",
            Self::UnsupportedTarget => "make-target-name-not-analyzed",
            Self::UnsupportedPrerequisite => "make-prerequisite-token-not-analyzed",
            Self::OrderOnlyPrerequisite => "make-order-only-semantics-not-analyzed",
            Self::VariableValue => "make-variable-value-not-analyzed",
            Self::DynamicTarget => "make-dynamic-target-not-analyzed",
            Self::DynamicPrerequisite => "make-dynamic-prerequisite-not-analyzed",
            Self::MultipleTargetPrerequisite => "make-multi-target-relations-not-analyzed",
        }
    }
}

struct Gap {
    start: usize,
    end: usize,
    kind: GapKind,
}

struct TokenScan {
    tokens: Vec<(usize, usize)>,
    unsupported: Option<(usize, usize)>,
    order_only: Option<usize>,
    dynamic: Option<(usize, usize)>,
}

fn record_gap(parsed: &mut ParsedSource, start: usize, end: usize, kind: GapKind) {
    parsed.skipped_regions = parsed.skipped_regions.saturating_add(1);
    parsed.last_gap_end = end;
    if parsed.gaps.len() < MAX_SCOPED_GAPS {
        parsed.gaps.push(Gap { start, end, kind });
    }
}

fn syntax_fact_count(parsed: &ParsedSource) -> usize {
    parsed.fact_count
}

fn scan_source(
    source: &[u8],
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<ParsedSource, AdapterError> {
    if max_syntax_nodes == 0 {
        return Err(failure("make-syntax-node-limit"));
    }
    std::str::from_utf8(source).map_err(|_| failure("invalid-utf8"))?;
    let mut parsed = ParsedSource {
        declarations: Vec::new(),
        skipped_regions: 0,
        fact_count: 1,
        gaps: Vec::new(),
        last_gap_end: 0,
        recipe_owner: None,
    };
    let mut line_start = 0;
    let mut continued = false;
    let mut define_depth = 0_usize;
    for (index, &byte) in source.iter().enumerate() {
        if index % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        if byte == b'\n' {
            scan_line(
                source.get(line_start..index).unwrap_or_default(),
                line_start,
                &mut continued,
                &mut define_depth,
                &mut parsed,
                max_syntax_nodes,
                cancellation,
            )?;
            line_start = index + 1;
        }
    }
    if line_start < source.len() {
        scan_line(
            source.get(line_start..).unwrap_or_default(),
            line_start,
            &mut continued,
            &mut define_depth,
            &mut parsed,
            max_syntax_nodes,
            cancellation,
        )?;
    }
    cancellation.check()?;
    Ok(parsed)
}

fn scan_line(
    line: &[u8],
    offset: usize,
    continued: &mut bool,
    define_depth: &mut usize,
    parsed: &mut ParsedSource,
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let raw_line_end = offset
        .checked_add(line.len())
        .ok_or_else(|| failure("make-source-offset-limit"))?;
    let code = line.get(..comment_start(line)).unwrap_or_default();
    let code = code.trim_ascii_end();
    let previous_continued = *continued;
    *continued = code.last() == Some(&b'\\')
        && code.iter().rev().take_while(|&&byte| byte == b'\\').count() % 2 == 1;
    let trimmed = code.trim_ascii_start();
    if trimmed.is_empty() {
        return Ok(());
    }
    if *define_depth > 0 {
        parsed.recipe_owner = None;
        record_gap(parsed, offset, raw_line_end, GapKind::DefineBlock);
        if trimmed == b"endef" {
            *define_depth -= 1;
        } else if trimmed.starts_with(b"define ") || trimmed.starts_with(b"define\t") {
            *define_depth = define_depth
                .checked_add(1)
                .ok_or_else(|| failure("make-define-depth-limit"))?;
        }
        return Ok(());
    }
    if previous_continued || *continued {
        parsed.recipe_owner = None;
        record_gap(parsed, offset, raw_line_end, GapKind::ContinuedHeader);
        return Ok(());
    }
    if line.first() == Some(&b'\t') {
        record_gap(parsed, offset, raw_line_end, GapKind::Recipe);
        if let Some(owner) = parsed.recipe_owner {
            let references = recipe_variable_references(
                line,
                offset,
                max_syntax_nodes.saturating_sub(parsed.fact_count),
                cancellation,
            )?;
            parsed.fact_count = parsed
                .fact_count
                .checked_add(references.len())
                .filter(|count| *count <= max_syntax_nodes)
                .ok_or_else(|| failure("make-syntax-node-limit"))?;
            let declaration = &mut parsed.declarations[owner];
            declaration
                .recipe_variables
                .try_reserve_exact(references.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            declaration.recipe_variables.extend(references);
            declaration.line_end = raw_line_end;
        }
        return Ok(());
    }
    parsed.recipe_owner = None;
    if trimmed.starts_with(b"define ") || trimmed.starts_with(b"define\t") {
        *define_depth = 1;
        record_gap(parsed, offset, raw_line_end, GapKind::DefineBlock);
        return Ok(());
    }
    let leading = code.len() - trimmed.len();
    let start = offset
        .checked_add(leading)
        .ok_or_else(|| failure("make-source-offset-limit"))?;
    let line_end = offset
        .checked_add(code.len())
        .ok_or_else(|| failure("make-source-offset-limit"))?;
    if let Some((name_start, name_end)) = variable_name(trimmed) {
        push_declaration(
            parsed,
            Declaration {
                kind: DeclarationKind::Variable,
                name_start: start + name_start,
                name_end: start + name_end,
                line_end,
                prerequisites: Vec::new(),
                recipe_variables: Vec::new(),
            },
            max_syntax_nodes,
        )?;
        if let Some(equals) = trimmed
            .get(name_end..)
            .and_then(|tail| tail.iter().position(|byte| *byte == b'='))
        {
            let value_start = name_end + equals + 1;
            if trimmed
                .get(value_start..)
                .is_some_and(|value| !value.trim_ascii().is_empty())
            {
                record_gap(
                    parsed,
                    start + value_start,
                    line_end,
                    GapKind::VariableValue,
                );
            }
        }
        return Ok(());
    }
    if let Some(colon) = trimmed.iter().position(|byte| *byte == b':') {
        let targets = static_tokens(
            trimmed.get(..colon).unwrap_or_default(),
            start,
            max_syntax_nodes,
        )?;
        if let Some((dynamic_start, dynamic_end)) = targets.dynamic {
            record_gap(
                parsed,
                dynamic_start,
                if targets.tokens.is_empty() {
                    line_end
                } else {
                    dynamic_end
                },
                GapKind::DynamicTarget,
            );
        }
        if !targets.tokens.is_empty() {
            if let Some((unsupported_start, unsupported_end)) = targets.unsupported {
                record_gap(
                    parsed,
                    unsupported_start,
                    unsupported_end,
                    GapKind::UnsupportedTarget,
                );
            }
            let prerequisite_start = colon + 1 + usize::from(trimmed.get(colon + 1) == Some(&b':'));
            let prerequisite_text = trimmed.get(prerequisite_start..).unwrap_or_default();
            let prerequisite_end = prerequisite_text
                .iter()
                .position(|byte| *byte == b';')
                .unwrap_or(prerequisite_text.len());
            let prerequisites = static_tokens(
                prerequisite_text
                    .get(..prerequisite_end)
                    .unwrap_or_default(),
                start + prerequisite_start,
                max_syntax_nodes,
            )?;
            if let Some((unsupported_start, unsupported_end)) = prerequisites.unsupported {
                record_gap(
                    parsed,
                    unsupported_start,
                    unsupported_end,
                    GapKind::UnsupportedPrerequisite,
                );
            }
            if let Some((dynamic_start, dynamic_end)) = prerequisites.dynamic {
                record_gap(
                    parsed,
                    dynamic_start,
                    dynamic_end,
                    GapKind::DynamicPrerequisite,
                );
            }
            if let Some(order_only_start) = prerequisites.order_only {
                record_gap(
                    parsed,
                    order_only_start,
                    line_end,
                    GapKind::OrderOnlyPrerequisite,
                );
            }
            let single_target = targets.tokens.len() == 1
                && targets.dynamic.is_none()
                && targets.unsupported.is_none();
            for (name_start, name_end) in targets.tokens {
                let references = if single_target {
                    prerequisites
                        .tokens
                        .iter()
                        .map(|&(start, end)| Reference { start, end })
                        .collect()
                } else {
                    Vec::new()
                };
                push_declaration(
                    parsed,
                    Declaration {
                        kind: DeclarationKind::Target,
                        name_start,
                        name_end,
                        line_end,
                        prerequisites: references,
                        recipe_variables: Vec::new(),
                    },
                    max_syntax_nodes,
                )?;
            }
            if single_target && variable_name(prerequisite_text.trim_ascii_start()).is_none() {
                parsed.recipe_owner = Some(parsed.declarations.len() - 1);
            }
            if prerequisite_end < prerequisite_text.len() {
                record_gap(
                    parsed,
                    start + prerequisite_start + prerequisite_end,
                    line_end,
                    GapKind::Recipe,
                );
            }
            if !single_target && !prerequisites.tokens.is_empty() {
                record_gap(
                    parsed,
                    start + prerequisite_start,
                    line_end,
                    GapKind::MultipleTargetPrerequisite,
                );
            }
            return Ok(());
        }
        if let Some((unsupported_start, unsupported_end)) = targets.unsupported {
            record_gap(
                parsed,
                unsupported_start,
                unsupported_end,
                GapKind::UnsupportedTarget,
            );
            return Ok(());
        }
        if targets.dynamic.is_some() {
            return Ok(());
        }
    }
    record_gap(parsed, offset, raw_line_end, GapKind::UnsupportedStatement);
    Ok(())
}

fn push_declaration(
    parsed: &mut ParsedSource,
    declaration: Declaration,
    max_syntax_nodes: usize,
) -> Result<(), AdapterError> {
    let next_count = parsed
        .fact_count
        .checked_add(2 + declaration.prerequisites.len() + declaration.recipe_variables.len())
        .filter(|count| *count <= max_syntax_nodes)
        .ok_or_else(|| failure("make-syntax-node-limit"))?;
    debug_assert!(next_count > 1);
    parsed
        .declarations
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    parsed.declarations.push(declaration);
    parsed.fact_count = next_count;
    Ok(())
}

// A written reference is independent of the recipe's shell or expanded value.
// Unsupported/computed expansions remain inside the retained whole-recipe gap.
fn recipe_variable_references(
    line: &[u8],
    offset: usize,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<Vec<Reference>, AdapterError> {
    let mut references = Vec::new();
    let mut index = 0;
    while index < line.len() {
        if index % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        if line[index] != b'$' {
            index += 1;
            continue;
        }
        if line.get(index + 1) == Some(&b'$') {
            index += 2;
            continue;
        }
        if !matches!(line.get(index + 1), Some(b'(' | b'{')) {
            index += 1;
            continue;
        }
        let start = index + 2;
        let closing = if line[index + 1] == b'(' { b')' } else { b'}' };
        let mut end = start;
        let mut depth = 1_usize;
        while end < line.len() {
            if end % CHECK_INTERVAL == 0 {
                cancellation.check()?;
            }
            match line[end] {
                b'(' | b'{' => {
                    depth = depth
                        .checked_add(1)
                        .ok_or_else(|| failure("make-expansion-depth-limit"))?
                }
                b')' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            end += 1;
        }
        if end == line.len() {
            break;
        }
        if line[end] == closing && valid_static_name(&line[start..end]) {
            if references.len() >= maximum {
                return Err(failure("make-syntax-node-limit"));
            }
            references
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            references.push(Reference {
                start: offset + start,
                end: offset + end,
            });
        }
        index = end + 1;
    }
    cancellation.check()?;
    Ok(references)
}

fn variable_name(line: &[u8]) -> Option<(usize, usize)> {
    let mut start = 0;
    loop {
        let tail = line.get(start..)?;
        let prefix = [b"export".as_slice(), b"override", b"private"]
            .into_iter()
            .find(|prefix| {
                tail.starts_with(prefix)
                    && tail.get(prefix.len()).is_some_and(u8::is_ascii_whitespace)
            });
        let Some(prefix) = prefix else {
            break;
        };
        start += prefix.len();
        start += line
            .get(start..)?
            .iter()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count();
    }
    let length = line
        .get(start..)?
        .iter()
        .take_while(|byte| static_name_byte(**byte))
        .count();
    let end = start.checked_add(length)?;
    if !valid_static_name(line.get(start..end)?) {
        return None;
    }
    let operator = line.get(end..)?.trim_ascii_start();
    [b"::=".as_slice(), b":=", b"?=", b"+=", b"!=", b"="]
        .iter()
        .any(|candidate| operator.starts_with(candidate))
        .then_some((start, end))
}

fn static_tokens(
    source: &[u8],
    offset: usize,
    max_syntax_nodes: usize,
) -> Result<TokenScan, AdapterError> {
    let mut scan = TokenScan {
        tokens: Vec::new(),
        unsupported: None,
        order_only: None,
        dynamic: None,
    };
    let mut index = 0;
    while index < source.len() {
        while source.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        let start = index;
        while source
            .get(index)
            .is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            index += 1;
        }
        if start == index {
            break;
        }
        let Some(token) = source.get(start..index) else {
            break;
        };
        if token == b"|" {
            scan.order_only.get_or_insert(offset + start);
            continue;
        }
        if token.first() == Some(&b';') {
            continue;
        }
        if token.contains(&b'$') {
            if let Some((_, end)) = &mut scan.dynamic {
                *end = offset + index;
            } else {
                scan.dynamic = Some((offset + start, offset + index));
            }
        } else if valid_static_name(token) {
            if scan.tokens.len() >= max_syntax_nodes {
                return Err(failure("make-syntax-node-limit"));
            }
            scan.tokens
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            scan.tokens.push((offset + start, offset + index));
        } else if let Some((_, end)) = &mut scan.unsupported {
            *end = offset + index;
        } else {
            scan.unsupported = Some((offset + start, offset + index));
        }
    }
    Ok(scan)
}

fn valid_static_name(name: &[u8]) -> bool {
    name.first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'.'))
        && name.iter().all(|byte| static_name_byte(*byte))
}

fn static_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
}

fn comment_start(line: &[u8]) -> usize {
    let mut backslashes = 0;
    for (index, &byte) in line.iter().enumerate() {
        if byte == b'#' && backslashes % 2 == 0 {
            return index;
        }
        backslashes = if byte == b'\\' { backslashes + 1 } else { 0 };
    }
    line.len()
}

fn source_span(file: FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    let start = u64::try_from(start).map_err(|_| failure("make-source-offset-limit"))?;
    let end = u64::try_from(end).map_err(|_| failure("make-source-offset-limit"))?;
    SourceSpan::new(file, start, end).map_err(|_| failure("make-source-span-invalid"))
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed Make syntax label")
}

fn failure(value: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(value).expect("fixed Make diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_headers_keep_exact_spans_without_mining_recipes_or_expansions() {
        let source = b"APP := app\n.PHONY: build check\nbuild: check $(DYNAMIC)\n\t@echo fake: target\ncheck:\n\t@echo ready\n";
        let parsed = scan_source(source, 16, &Cancellation::new()).unwrap();
        let names: Vec<_> = parsed
            .declarations
            .iter()
            .map(|declaration| &source[declaration.name_start..declaration.name_end])
            .collect();
        assert_eq!(
            names,
            [
                b"APP".as_slice(),
                b".PHONY".as_slice(),
                b"build".as_slice(),
                b"check".as_slice(),
            ]
        );
        assert_eq!(parsed.declarations[1].prerequisites.len(), 2);
        assert_eq!(parsed.declarations[2].prerequisites.len(), 1);
        assert_eq!(syntax_fact_count(&parsed), 12);
        assert!(parsed.skipped_regions >= 3);
        assert!(parsed.gaps.iter().any(|gap| {
            gap.kind.code() == "make-dynamic-prerequisite-not-analyzed"
                && &source[gap.start..gap.end] == b"$(DYNAMIC)"
        }));
        assert!(parsed.gaps.iter().any(|gap| {
            gap.kind.code() == "make-recipe-not-analyzed"
                && &source[gap.start..gap.end] == b"\t@echo fake: target"
        }));
        assert!(scan_source(source, 11, &Cancellation::new()).is_err());
    }

    #[test]
    fn continued_headers_and_define_bodies_cannot_invent_targets() {
        let source =
            b"define SCRIPT\nphantom: target\nendef\nreal: \\\n fake: target\n\t@echo ready\n";
        let parsed = scan_source(source, 16, &Cancellation::new()).unwrap();
        assert!(parsed.declarations.is_empty());
        assert!(parsed.skipped_regions >= 4);
    }

    #[test]
    fn nested_define_bodies_do_not_escape_into_static_targets() {
        let source = b"define OUTER\ndefine INNER\nphantom: target\nendef\nstill_phantom: target\nendef\nreal: target\n";
        let parsed = scan_source(source, 8, &Cancellation::new()).unwrap();
        assert_eq!(parsed.declarations.len(), 1);
        let declaration = &parsed.declarations[0];
        assert_eq!(
            &source[declaration.name_start..declaration.name_end],
            b"real"
        );
        assert!(parsed.skipped_regions >= 4);
    }

    #[test]
    fn recipe_text_cannot_open_a_define_block() {
        let source = b"all:\n\tdefine PHANTOM\nreal: check\ncheck:\n";
        let parsed = scan_source(source, 8, &Cancellation::new()).unwrap();
        let names: Vec<_> = parsed
            .declarations
            .iter()
            .map(|declaration| &source[declaration.name_start..declaration.name_end])
            .collect();
        assert_eq!(names, [b"all".as_slice(), b"real", b"check"]);
        assert_eq!(parsed.gaps[0].kind.code(), "make-recipe-not-analyzed");
    }

    #[test]
    fn many_unsupported_lines_preserve_a_bounded_gap_summary_range() {
        let source = b"include first.mk\n".repeat(MAX_SCOPED_GAPS + 8);
        let parsed = scan_source(&source, 1, &Cancellation::new()).unwrap();
        assert_eq!(parsed.skipped_regions, MAX_SCOPED_GAPS + 8);
        assert_eq!(parsed.gaps.len(), MAX_SCOPED_GAPS);
        assert_eq!(parsed.last_gap_end, source.len() - 1);
        assert_eq!(parsed.gaps[0].kind.code(), "make-statement-not-analyzed");
    }

    #[test]
    fn static_path_names_are_retained_with_order_only_semantics_as_a_gap() {
        let source = b"ROOT := src\nobj/main.o: src/main.c | generated/\n";
        let parsed = scan_source(source, 8, &Cancellation::new()).unwrap();
        let targets: Vec<_> = parsed
            .declarations
            .iter()
            .filter(|declaration| declaration.kind == DeclarationKind::Target)
            .map(|declaration| &source[declaration.name_start..declaration.name_end])
            .collect();
        assert_eq!(targets, [b"obj/main.o".as_slice()]);
        let prerequisites: Vec<_> = parsed.declarations[1]
            .prerequisites
            .iter()
            .map(|reference| &source[reference.start..reference.end])
            .collect();
        assert_eq!(prerequisites, [b"src/main.c".as_slice(), b"generated/"]);
        let observed: Vec<_> = parsed
            .gaps
            .iter()
            .map(|gap| (gap.kind.code(), &source[gap.start..gap.end]))
            .collect();
        assert!(observed.contains(&("make-variable-value-not-analyzed", b" src".as_slice())));
        assert!(
            !observed
                .iter()
                .any(|(code, _)| *code == "make-target-name-not-analyzed")
        );
        assert!(
            observed
                .iter()
                .any(|(code, _)| *code == "make-order-only-semantics-not-analyzed")
        );
        assert_eq!(parsed.declarations.len(), 2);

        let source = b"all: check | generated/\ncheck:\n";
        let parsed = scan_source(source, 8, &Cancellation::new()).unwrap();
        assert!(
            !parsed
                .gaps
                .iter()
                .any(|gap| { gap.kind.code() == "make-prerequisite-token-not-analyzed" })
        );
        assert!(parsed.gaps.iter().any(|gap| {
            gap.kind.code() == "make-order-only-semantics-not-analyzed"
                && &source[gap.start..gap.end] == b"| generated/"
        }));

        let source = b"$(TARGET): source.c\n";
        let parsed = scan_source(source, 8, &Cancellation::new()).unwrap();
        assert!(parsed.declarations.is_empty());
        assert_eq!(parsed.gaps.len(), 1);
        assert_eq!(
            parsed.gaps[0].kind.code(),
            "make-dynamic-target-not-analyzed"
        );
        assert_eq!(
            &source[parsed.gaps[0].start..parsed.gaps[0].end],
            b"$(TARGET): source.c"
        );
    }

    #[test]
    fn gap_codes_fit_the_conservative_stream_budget() {
        for kind in [
            GapKind::ContinuedHeader,
            GapKind::DefineBlock,
            GapKind::Recipe,
            GapKind::UnsupportedStatement,
            GapKind::UnsupportedTarget,
            GapKind::UnsupportedPrerequisite,
            GapKind::OrderOnlyPrerequisite,
            GapKind::VariableValue,
            GapKind::DynamicTarget,
            GapKind::DynamicPrerequisite,
            GapKind::MultipleTargetPrerequisite,
        ] {
            assert!(kind.code().len() <= MAX_GAP_CODE_BYTES);
        }
        assert!("make-gap-diagnostic-limit".len() <= MAX_GAP_CODE_BYTES);
    }

    #[test]
    fn literal_recipe_variables_retain_exact_owner_spans_and_gap() {
        let source = b"FRONTEND := frontend\nbuild: static $(STAMP)\n\tnpm --prefix $(FRONTEND) run ${FRONTEND}\nnext:\n\t@echo $$(FRONTEND) $(PREFIX_$(KEY)) $(shell echo BAD)\n";
        let parsed = scan_source(source, 32, &Cancellation::new()).unwrap();
        let target = &parsed.declarations[1];
        let references: Vec<_> = target
            .recipe_variables
            .iter()
            .map(|site| &source[site.start..site.end])
            .collect();
        assert_eq!(references, [b"FRONTEND".as_slice(), b"FRONTEND"]);
        assert_eq!(&source[target.name_start..target.name_end], b"build");
        assert_eq!(source[target.line_end], b'\n');
        assert!(parsed.declarations[2].recipe_variables.is_empty());
        assert!(
            parsed
                .gaps
                .iter()
                .any(|gap| gap.kind.code() == "make-recipe-not-analyzed")
        );
        assert_eq!(syntax_fact_count(&parsed), 10);
        assert!(scan_source(source, 9, &Cancellation::new()).is_err());
    }

    #[test]
    fn ambiguous_dynamic_and_target_specific_headers_cannot_own_recipe_uses() {
        for source in [
            b"APP := app\nfirst second:\n\t@echo $(APP)\n".as_slice(),
            b"APP := app\n$(TARGET):\n\t@echo $(APP)\n",
            b"APP := app\nfirst: APP := override\n\t@echo $(APP)\n",
            b"APP := app\nfirst: \\\n continued\n\t@echo $(APP)\n",
        ] {
            let parsed = scan_source(source, 32, &Cancellation::new()).unwrap();
            assert!(
                parsed
                    .declarations
                    .iter()
                    .all(|row| row.recipe_variables.is_empty())
            );
            assert!(parsed.skipped_regions > 0);
        }
    }

    #[test]
    fn literal_recipe_scan_observes_cancellation_and_exact_quota() {
        let cancellation = Cancellation::new();
        let line = b"\t@echo $(FIRST) ${SECOND}";
        assert_eq!(
            recipe_variable_references(line, 0, 2, &cancellation)
                .unwrap()
                .len(),
            2
        );
        assert!(recipe_variable_references(line, 0, 1, &cancellation).is_err());
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(recipe_variable_references(line, 0, 2, &cancellation).is_err());
    }

    #[test]
    fn inline_recipes_are_not_prerequisites() {
        let source = b"all: check; @echo phantom\ncheck:\n";
        let parsed = scan_source(source, 8, &Cancellation::new()).unwrap();
        assert_eq!(parsed.declarations.len(), 2);
        let references = &parsed.declarations[0].prerequisites;
        assert_eq!(references.len(), 1);
        assert_eq!(&source[references[0].start..references[0].end], b"check");
    }
}
