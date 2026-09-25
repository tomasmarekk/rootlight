//! First-party Agda source scanner for explicit top-level declarations.
//! Layout scopes, mixfix applications, and type checking remain bounded.

use std::collections::BTreeSet;

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

const CHECK_INTERVAL: usize = 64;
const GAP_CODE_BYTES: usize = 48;
const GAP_ACCOUNTED_BYTES: usize = 256;

/// Bounded parser for source-backed Agda modules, imports, and declarations.
pub struct AgdaParseProvider {
    capabilities: ParseCapabilities,
}

impl AgdaParseProvider {
    /// Returns the content identity of this first-party frontend.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("agda.rs"))
    }

    /// Creates a parser with caller-owned source and syntax-node ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError`] when a ceiling is zero.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        let language = LanguageId::new("agda").expect("fixed Agda language label");
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

    /// Counts the facts emitted by parsing before stream admission.
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
            request.source().path().as_str(),
            request
                .limits()
                .max_syntax_nodes()
                .min(self.capabilities.max_syntax_nodes()),
            cancellation,
        )?;
        Ok(parsed.fact_count)
    }
}

impl ParseProvider for AgdaParseProvider {
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
            source.path().as_str(),
            request
                .limits()
                .max_syntax_nodes()
                .min(self.capabilities.max_syntax_nodes()),
            cancellation,
        )?;
        let file = source.source_ref().span().file();
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(parsed.fact_count)
            .map_err(|_| SinkError::AllocationFailed)?;
        facts.push(SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.source_ref().span(),
            0,
            label("agda.source_file"),
        ));
        let mut next_id = 2_u64;
        let module_id = if let Some(module) = parsed.module {
            facts.push(SyntaxFact::new(
                next_id,
                Some(1),
                SyntaxFactKind::Module,
                source.source_ref().span(),
                1,
                label("agda.module.declaration"),
            ));
            facts.push(SyntaxFact::new(
                next_id + 1,
                Some(next_id),
                SyntaxFactKind::Occurrence,
                source_span(file, module.name_start, module.name_end)?,
                2,
                label("agda.module.definition"),
            ));
            next_id += 2;
            Some(2)
        } else {
            None
        };
        let parent = module_id.unwrap_or(1);
        let depth = if module_id.is_some() { 2 } else { 1 };
        for item in &parsed.items {
            cancellation.check()?;
            match item.kind {
                ItemKind::Import => {
                    facts.push(SyntaxFact::new(
                        next_id,
                        Some(parent),
                        SyntaxFactKind::Import,
                        source_span(file, item.name_start, item.name_end)?,
                        depth,
                        label("agda.module.import"),
                    ));
                    next_id += 1;
                }
                kind => {
                    let (declaration, signature, definition) = match kind {
                        ItemKind::Function => (
                            "agda.function.declaration",
                            "agda.function.signature",
                            "agda.function.definition",
                        ),
                        ItemKind::Data => (
                            "agda.data.declaration",
                            "agda.data.signature",
                            "agda.data.definition",
                        ),
                        ItemKind::Record => (
                            "agda.record.declaration",
                            "agda.record.signature",
                            "agda.record.definition",
                        ),
                        ItemKind::Import => unreachable!("handled above"),
                    };
                    facts.push(SyntaxFact::new(
                        next_id,
                        Some(parent),
                        SyntaxFactKind::Declaration,
                        source_span(file, item.line_start, item.line_end)?,
                        depth,
                        label(declaration),
                    ));
                    facts.push(SyntaxFact::new(
                        next_id + 1,
                        Some(next_id),
                        SyntaxFactKind::Signature,
                        source_span(file, item.line_start, item.line_end)?,
                        depth + 1,
                        label(signature),
                    ));
                    facts.push(SyntaxFact::new(
                        next_id + 2,
                        Some(next_id),
                        SyntaxFactKind::Occurrence,
                        source_span(file, item.name_start, item.name_end)?,
                        depth + 1,
                        label(definition),
                    ));
                    next_id += 3;
                }
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
        if !source.bytes().is_empty() && can_emit_gap(request, usage) {
            let code = if parsed.lexical_incomplete {
                "agda-lexical-structure-partial"
            } else if parsed.literate_incomplete {
                "agda-literate-structure-partial"
            } else {
                "agda-layout-and-expression-semantics-partial"
            };
            let reference = source.source_ref();
            let diagnostic = AdapterDiagnostic::new(
                DiagnosticCode::new(code).expect("fixed Agda diagnostic code"),
                DiagnosticSeverity::Warning,
                Some(SourceRef::new(
                    reference.repository(),
                    reference.generation(),
                    reference.span(),
                    reference.content_hash(),
                    None,
                )),
                CoverageStatus::Bounded,
            );
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), Vec::new(), vec![diagnostic]),
                cancellation,
            )?;
        }
        let usage = sink.staged_usage();
        let bytes = source.bytes().len();
        let entity_count = parsed
            .items
            .iter()
            .filter(|item| item.kind != ItemKind::Import)
            .count()
            + usize::from(parsed.module.is_some());
        let import_count = parsed
            .items
            .iter()
            .filter(|item| item.kind == ItemKind::Import)
            .count();
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Bounded,
            bytes,
            bytes,
            usize::from(bytes > 0),
            vec![
                DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                DomainCoverage::new(
                    FactDomain::Entities,
                    CoverageStatus::Bounded,
                    entity_count,
                    entity_count,
                    0,
                )?,
                DomainCoverage::new(
                    FactDomain::Occurrences,
                    CoverageStatus::Bounded,
                    import_count,
                    import_count,
                    0,
                )?,
            ],
        )?;
        Ok(ParseReport::new(
            coverage,
            ResourceUsage::new(bytes, facts.len(), facts.len(), 2, None, usage),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

fn can_emit_gap(request: &ParseRequest<'_>, used: StreamUsage) -> bool {
    let syntax = request.limits().syntax_stream();
    let batch = syntax.batch();
    let ir = request.limits().ir();
    used.batches() < syntax.max_batches()
        && used.diagnostics() < syntax.max_diagnostics()
        && syntax
            .max_diagnostic_bytes()
            .saturating_sub(used.diagnostic_bytes())
            >= GAP_CODE_BYTES
        && syntax
            .max_string_bytes()
            .saturating_sub(used.string_bytes())
            >= GAP_CODE_BYTES
        && syntax
            .max_output_bytes()
            .saturating_sub(used.output_bytes())
            >= GAP_ACCOUNTED_BYTES
        && batch.max_diagnostics() > 0
        && batch.max_diagnostic_bytes() >= GAP_CODE_BYTES
        && batch.max_output_bytes() >= GAP_ACCOUNTED_BYTES
        && ir.max_diagnostics > 0
        && ir.max_skipped_regions > 0
        && ir.max_total_diagnostic_bytes >= 128
}

#[derive(Clone, Copy)]
struct Module {
    name_start: usize,
    name_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Import,
    Function,
    Data,
    Record,
}

#[derive(Clone, Copy)]
struct Item {
    kind: ItemKind,
    line_start: usize,
    line_end: usize,
    name_start: usize,
    name_end: usize,
}

struct ParsedSource {
    module: Option<Module>,
    items: Vec<Item>,
    fact_count: usize,
    lexical_incomplete: bool,
    literate_incomplete: bool,
}

fn scan_source(
    source: &[u8],
    path: &str,
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<ParsedSource, AdapterError> {
    if max_syntax_nodes == 0 {
        return Err(failure("agda-syntax-node-limit"));
    }
    std::str::from_utf8(source).map_err(|_| failure("invalid-utf8"))?;
    let (mut code, literate_incomplete) = literate_code(source, path, cancellation)?;
    let lexical_incomplete = mask_non_code(&mut code, cancellation)?;
    let base_indent = lines(&code)
        .filter_map(|(_, line)| {
            let indent = line
                .iter()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
            (indent < line.len() && line[indent] != b'\r').then_some(indent)
        })
        .min();
    let mut module = None;
    let mut items = Vec::new();
    let mut seen_names = BTreeSet::new();
    let mut fact_count = 1_usize;
    for (line_number, (line_start, line)) in lines(&code).enumerate() {
        if line_number % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        let indent = line
            .iter()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
        if Some(indent) != base_indent || indent == line.len() {
            continue;
        }
        let line_end = line_start + line.trim_ascii_end().len();
        let first_start = line_start + indent;
        let first_end = token_end(&code, first_start, line_end);
        let first = &code[first_start..first_end];
        if first == b"module" && module.is_none() {
            let name_start = skip_space(&code, first_end, line_end);
            let name_end = token_end(&code, name_start, line_end);
            if valid_qualified_name(&code[name_start..name_end])
                && code[name_end..line_end]
                    .split(|byte| byte.is_ascii_whitespace())
                    .any(|token| token == b"where")
            {
                module = Some(Module {
                    name_start,
                    name_end,
                });
                fact_count = checked_fact_count(fact_count, 2, max_syntax_nodes)?;
            }
            continue;
        }
        let (kind, name_start) = if first == b"open" {
            let second_start = skip_space(&code, first_end, line_end);
            let second_end = token_end(&code, second_start, line_end);
            if &code[second_start..second_end] != b"import" {
                continue;
            }
            (ItemKind::Import, skip_space(&code, second_end, line_end))
        } else if first == b"import" {
            (ItemKind::Import, skip_space(&code, first_end, line_end))
        } else if first == b"data" {
            (ItemKind::Data, skip_space(&code, first_end, line_end))
        } else if first == b"record" {
            (ItemKind::Record, skip_space(&code, first_end, line_end))
        } else if !reserved(first) {
            let second_start = skip_space(&code, first_end, line_end);
            let second_end = token_end(&code, second_start, line_end);
            let signature = &code[second_start..second_end] == b":";
            let equation = code[second_start..line_end]
                .split(|byte| byte.is_ascii_whitespace())
                .any(|token| token == b"=");
            if !signature && !equation {
                continue;
            }
            (ItemKind::Function, first_start)
        } else {
            continue;
        };
        let name_end = token_end(&code, name_start, line_end);
        let name = &code[name_start..name_end];
        let valid = if kind == ItemKind::Import {
            valid_qualified_name(name)
        } else {
            valid_name(name)
        };
        if !valid || kind != ItemKind::Import && !seen_names.insert(name.to_vec()) {
            continue;
        }
        fact_count = checked_fact_count(
            fact_count,
            if kind == ItemKind::Import { 1 } else { 3 },
            max_syntax_nodes,
        )?;
        items.push(Item {
            kind,
            line_start: first_start,
            line_end,
            name_start,
            name_end,
        });
    }
    Ok(ParsedSource {
        module,
        items,
        fact_count,
        lexical_incomplete,
        literate_incomplete,
    })
}

fn checked_fact_count(
    count: usize,
    additional: usize,
    maximum: usize,
) -> Result<usize, AdapterError> {
    count
        .checked_add(additional)
        .filter(|total| *total <= maximum)
        .ok_or_else(|| failure("agda-syntax-node-limit"))
}

fn valid_qualified_name(name: &[u8]) -> bool {
    name.split(|byte| *byte == b'.').all(valid_name)
}

fn valid_name(name: &[u8]) -> bool {
    let Ok(name) = std::str::from_utf8(name) else {
        return false;
    };
    !name.is_empty()
        && name != "_"
        && !matches!(name, "λ" | "∀" | "→")
        && !reserved(name.as_bytes())
        && name.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '.' | ';' | '{' | '}' | '(' | ')' | '@' | '"')
        })
}

fn reserved(token: &[u8]) -> bool {
    matches!(
        token,
        b"abstract"
            | b"constructor"
            | b"data"
            | b"field"
            | b"import"
            | b"infix"
            | b"infixl"
            | b"infixr"
            | b"instance"
            | b"let"
            | b"macro"
            | b"module"
            | b"mutual"
            | b"open"
            | b"opaque"
            | b"pattern"
            | b"postulate"
            | b"primitive"
            | b"private"
            | b"public"
            | b"record"
            | b"rewrite"
            | b"syntax"
            | b"variable"
            | b"where"
            | b"with"
            | b"="
            | b":"
            | b"?"
            | b"|"
            | b"->"
    )
}

fn literate_code(
    source: &[u8],
    path: &str,
    cancellation: &Cancellation,
) -> Result<(Vec<u8>, bool), AdapterError> {
    let tex = path.ends_with(".lagda") || path.ends_with(".lagda.tex");
    let markdown = path.ends_with(".lagda.md");
    let rst = path.ends_with(".lagda.rst");
    let mut code = Vec::new();
    code.try_reserve_exact(source.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    code.extend_from_slice(source);
    if !tex && !markdown && !rst {
        return Ok((code, false));
    }
    for byte in &mut code {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
    let mut in_code = false;
    let mut rst_indent = None;
    let mut rst_armed = None;
    for (line_number, (start, line)) in lines(source).enumerate() {
        if line_number % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        let trimmed = line.trim_ascii();
        if tex {
            if !in_code && trimmed.starts_with(b"\\begin{code}") {
                in_code = true;
            } else if in_code && trimmed.starts_with(b"\\end{code}") {
                in_code = false;
            } else if in_code {
                code[start..start + line.len()].copy_from_slice(line);
            }
        } else if markdown {
            if !in_code && matches!(trimmed, b"```" | b"```agda") {
                in_code = true;
            } else if in_code && trimmed == b"```" {
                in_code = false;
            } else if in_code {
                code[start..start + line.len()].copy_from_slice(line);
            }
        } else {
            let indent = line
                .iter()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
            if let Some(block_indent) = rst_indent {
                if trimmed.is_empty() || indent > block_indent {
                    code[start..start + line.len()].copy_from_slice(line);
                    continue;
                }
                rst_indent = None;
            }
            if let Some(marker_indent) = rst_armed
                && !trimmed.is_empty()
                && indent > marker_indent
            {
                rst_indent = Some(marker_indent);
                code[start..start + line.len()].copy_from_slice(line);
                rst_armed = None;
                continue;
            }
            if !trimmed.is_empty() {
                rst_armed =
                    (!trimmed.starts_with(b"..") && trimmed.ends_with(b"::")).then_some(indent);
            }
        }
    }
    Ok((code, in_code || rst_armed.is_some()))
}

fn mask_non_code(code: &mut [u8], cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut cursor = 0;
    let mut depth = 0_usize;
    let mut string = false;
    if code.starts_with(b"\xef\xbb\xbf") {
        mask_range(code, 0, 3);
        cursor = 3;
    }
    while cursor < code.len() {
        if cursor % 4096 == 0 {
            cancellation.check()?;
        }
        if depth > 0 {
            if code[cursor..].starts_with(b"{-") {
                depth = depth.saturating_add(1);
                mask_range(code, cursor, cursor + 2);
                cursor += 2;
            } else if code[cursor..].starts_with(b"-}") {
                depth -= 1;
                mask_range(code, cursor, cursor + 2);
                cursor += 2;
            } else {
                mask_range(code, cursor, cursor + 1);
                cursor += 1;
            }
        } else if string {
            if code[cursor] == b'\\' && cursor + 1 < code.len() {
                mask_range(code, cursor, cursor + 2);
                cursor += 2;
            } else if code[cursor] == b'"' {
                mask_range(code, cursor, cursor + 1);
                string = false;
                cursor += 1;
            } else {
                mask_range(code, cursor, cursor + 1);
                cursor += 1;
            }
        } else if code[cursor..].starts_with(b"{-") {
            depth = 1;
            mask_range(code, cursor, cursor + 2);
            cursor += 2;
        } else if code[cursor..].starts_with(b"--") {
            let end = code[cursor..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(code.len(), |relative| cursor + relative);
            mask_range(code, cursor, end);
            cursor = end;
        } else if code[cursor] == b'"' {
            string = true;
            mask_range(code, cursor, cursor + 1);
            cursor += 1;
        } else {
            cursor += 1;
        }
    }
    Ok(depth > 0 || string)
}

fn mask_range(code: &mut [u8], start: usize, end: usize) {
    for byte in &mut code[start..end] {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
}

fn lines(source: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut offset = 0;
    source
        .split_inclusive(|byte| *byte == b'\n')
        .map(move |line| {
            let start = offset;
            offset += line.len();
            (start, line.trim_ascii_end())
        })
}

fn token_end(source: &[u8], start: usize, limit: usize) -> usize {
    (start..limit)
        .find(|&index| source[index].is_ascii_whitespace())
        .unwrap_or(limit)
}

fn skip_space(source: &[u8], mut start: usize, limit: usize) -> usize {
    while start < limit && source[start].is_ascii_whitespace() {
        start += 1;
    }
    start
}

fn source_span(file: FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    let start = u64::try_from(start).map_err(|_| failure("agda-source-offset-limit"))?;
    let end = u64::try_from(end).map_err(|_| failure("agda-source-offset-limit"))?;
    SourceSpan::new(file, start, end).map_err(|_| failure("agda-source-span-invalid"))
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed Agda syntax label")
}

fn failure(value: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(value).expect("fixed Agda diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_unicode_names_without_comments_or_nested_definitions() {
        let source = "{-# OPTIONS --safe #-}\nopen import Prelude\nmodule Sample where\n{- outer {- module Ghost where -} -}\n_∙_ : Nat → Nat → Nat\n_∙_ x y = x\nprivate\n  hidden : Nat\ndata Choice : Set where\n  yes : Choice\nrecord Pair : Set where\n  field left : Nat\n";
        let parsed = scan_source(source.as_bytes(), "Sample.agda", 64, &Cancellation::new())
            .expect("bounded source");
        assert_eq!(
            &source[parsed.module.expect("module").name_start
                ..parsed.module.expect("module").name_end],
            "Sample"
        );
        let captures = parsed
            .items
            .iter()
            .map(|item| (&source[item.name_start..item.name_end], item.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            captures,
            [
                ("Prelude", ItemKind::Import),
                ("_∙_", ItemKind::Function),
                ("Choice", ItemKind::Data),
                ("Pair", ItemKind::Record)
            ]
        );
        assert_eq!(parsed.fact_count, 1 + 2 + 1 + 3 * 3);
    }

    #[test]
    fn literate_markdown_ignores_prose_and_unrelated_fences() {
        let source = b"module Ghost where\n```text\nmodule Wrong where\n```\n```agda\nmodule Real where\nreal : Set\n```\n";
        let parsed = scan_source(source, "Guide.lagda.md", 32, &Cancellation::new())
            .expect("bounded literate source");
        let module = parsed.module.expect("code module");
        assert_eq!(&source[module.name_start..module.name_end], b"Real");
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(
            &source[parsed.items[0].name_start..parsed.items[0].name_end],
            b"real"
        );
    }

    #[test]
    fn literate_tex_and_rst_keep_only_explicit_code() {
        for (path, source, expected) in [
            (
                "Guide.lagda.tex",
                "module Ghost where\n\\begin{code}\nmodule TeX where\nvalue : Set\n\\end{code}\nmodule Hidden where\n",
                "TeX",
            ),
            (
                "Guide.lagda.rst",
                "module Ghost where\n\n::\n\n  module ReST where\n  value : Set\n\nAfter prose\n",
                "ReST",
            ),
        ] {
            let parsed = scan_source(source.as_bytes(), path, 32, &Cancellation::new())
                .expect("bounded literate source");
            let module = parsed.module.expect("code module");
            assert_eq!(&source[module.name_start..module.name_end], expected);
            assert_eq!(parsed.items.len(), 1);
            assert_eq!(
                &source[parsed.items[0].name_start..parsed.items[0].name_end],
                "value"
            );
        }
    }

    #[test]
    fn quoted_comment_markers_do_not_hide_following_declarations() {
        let source = "module Sample where\nquoted = \"{- literal -} -- literal\"\nnext : Set\n";
        let parsed = scan_source(source.as_bytes(), "Sample.agda", 24, &Cancellation::new())
            .expect("bounded source");
        let names = parsed
            .items
            .iter()
            .map(|item| &source[item.name_start..item.name_end])
            .collect::<Vec<_>>();
        assert_eq!(names, ["quoted", "next"]);
        assert!(!parsed.lexical_incomplete);
    }

    #[test]
    fn syntax_limit_and_invalid_utf8_are_rejected() {
        let source = b"module Good where\nvalue : Set\n";
        assert!(scan_source(source, "Good.agda", 5, &Cancellation::new()).is_err());
        assert!(scan_source(b"", "Empty.agda", 0, &Cancellation::new()).is_err());
        assert!(scan_source(b"\xff", "Bad.agda", 8, &Cancellation::new()).is_err());
    }
}
