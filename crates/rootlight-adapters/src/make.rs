//! First-party structural parser for static Makefile declarations.
//! It preserves authored byte spans for variables, targets, and static prerequisites.
//! Expansion, recipes, directives, and continued headers remain bounded gaps.

use rootlight_adapter_sdk::{
    AdapterError, CoverageReport, DescriptorError, DiagnosticCode, DomainCoverage, EncodingId,
    LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider, ParseReport, ParseRequest,
    ResourceUsage, SinkError, StreamEnd, SyntaxFact, SyntaxFactBatch, SyntaxFactKind,
    SyntaxFactSink, SyntaxKindLabel,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId, content_hash};
use rootlight_ir::{AnalysisTier, CoverageStatus, FactDomain, SourceSpan};

const CHECK_INTERVAL: usize = 4096;

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
        let usage = sink.staged_usage();
        let source_bytes = source.bytes().len();
        let reference_count: usize = parsed
            .declarations
            .iter()
            .map(|declaration| declaration.prerequisites.len())
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
}

struct ParsedSource {
    declarations: Vec<Declaration>,
    skipped_regions: usize,
    fact_count: usize,
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
) -> Result<(), AdapterError> {
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
        parsed.skipped_regions = parsed.skipped_regions.saturating_add(1);
        if trimmed == b"endef" {
            *define_depth -= 1;
        } else if trimmed.starts_with(b"define ") || trimmed.starts_with(b"define\t") {
            *define_depth = define_depth
                .checked_add(1)
                .ok_or_else(|| failure("make-define-depth-limit"))?;
        }
        return Ok(());
    }
    if previous_continued || *continued || line.first() == Some(&b'\t') {
        parsed.skipped_regions = parsed.skipped_regions.saturating_add(1);
        return Ok(());
    }
    if trimmed.starts_with(b"define ") || trimmed.starts_with(b"define\t") {
        *define_depth = 1;
        parsed.skipped_regions = parsed.skipped_regions.saturating_add(1);
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
            },
            max_syntax_nodes,
        )?;
        return Ok(());
    }
    if let Some(colon) = trimmed.iter().position(|byte| *byte == b':') {
        let targets = static_tokens(
            trimmed.get(..colon).unwrap_or_default(),
            start,
            max_syntax_nodes,
        )?;
        if !targets.is_empty() {
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
            let single_target = targets.len() == 1;
            for (name_start, name_end) in targets {
                let references = if single_target {
                    prerequisites
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
                    },
                    max_syntax_nodes,
                )?;
            }
            if !single_target
                || trimmed
                    .get(prerequisite_start..)
                    .is_some_and(|tail| tail.contains(&b'$'))
            {
                parsed.skipped_regions = parsed.skipped_regions.saturating_add(1);
            }
            return Ok(());
        }
    }
    parsed.skipped_regions = parsed.skipped_regions.saturating_add(1);
    Ok(())
}

fn push_declaration(
    parsed: &mut ParsedSource,
    declaration: Declaration,
    max_syntax_nodes: usize,
) -> Result<(), AdapterError> {
    let next_count = parsed
        .fact_count
        .checked_add(2 + declaration.prerequisites.len())
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
) -> Result<Vec<(usize, usize)>, AdapterError> {
    let mut tokens = Vec::new();
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
        if token == b"|" || token.first() == Some(&b';') {
            continue;
        }
        if valid_static_name(token) {
            if tokens.len() >= max_syntax_nodes {
                return Err(failure("make-syntax-node-limit"));
            }
            tokens
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            tokens.push((offset + start, offset + index));
        }
    }
    Ok(tokens)
}

fn valid_static_name(name: &[u8]) -> bool {
    name.first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'.'))
        && name.iter().all(|byte| static_name_byte(*byte))
}

fn static_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
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
    fn inline_recipes_are_not_prerequisites() {
        let source = b"all: check; @echo phantom\ncheck:\n";
        let parsed = scan_source(source, 8, &Cancellation::new()).unwrap();
        assert_eq!(parsed.declarations.len(), 2);
        let references = &parsed.declarations[0].prerequisites;
        assert_eq!(references.len(), 1);
        assert_eq!(&source[references[0].start..references[0].end], b"check");
    }
}
