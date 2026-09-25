//! First-party AWK source parser for authored function definitions and direct calls.
//! It retains exact byte spans without executing rules or resolving dynamic calls.
//! Unparsed expression semantics remain an explicit source-scoped coverage gap.

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
const GAP_CODE_BYTES: usize = 40;
const GAP_ACCOUNTED_BYTES: usize = 256;

/// Bounded native provider for source-backed AWK function identities.
pub struct AwkParseProvider {
    capabilities: ParseCapabilities,
}

impl AwkParseProvider {
    /// Returns the content identity of this first-party frontend.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("awk.rs"))
    }

    /// Creates the parser under caller-owned source and syntax-node ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError`] when a ceiling is zero.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed `awk` language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        let language = LanguageId::new("awk").expect("fixed AWK language label");
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

    /// Counts exactly the syntax facts that parsing will emit before admission.
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
        Ok(parsed.fact_count)
    }
}

impl ParseProvider for AwkParseProvider {
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
            label("awk.source_file"),
        ));
        for (index, function) in parsed.functions.iter().enumerate() {
            cancellation.check()?;
            let declaration_id = u64::try_from(index)
                .ok()
                .and_then(|index| index.checked_mul(3))
                .and_then(|offset| offset.checked_add(2))
                .ok_or_else(|| failure("awk-syntax-node-limit"))?;
            facts.push(SyntaxFact::new(
                declaration_id,
                Some(1),
                SyntaxFactKind::Declaration,
                source_span(file, function.start, function.end)?,
                1,
                label("awk.function.declaration"),
            ));
            facts.push(SyntaxFact::new(
                declaration_id + 1,
                Some(declaration_id),
                SyntaxFactKind::Signature,
                source_span(file, function.start, function.body_start)?,
                2,
                label("awk.function.signature"),
            ));
            facts.push(SyntaxFact::new(
                declaration_id + 2,
                Some(declaration_id),
                SyntaxFactKind::Occurrence,
                source_span(file, function.name_start, function.name_end)?,
                2,
                label("awk.function.definition"),
            ));
        }
        let call_base = u64::try_from(parsed.functions.len())
            .ok()
            .and_then(|count| count.checked_mul(3))
            .and_then(|count| count.checked_add(2))
            .ok_or_else(|| failure("awk-syntax-node-limit"))?;
        for (index, call) in parsed.calls.iter().enumerate() {
            cancellation.check()?;
            let local_id = call_base
                .checked_add(u64::try_from(index).map_err(|_| failure("awk-syntax-node-limit"))?)
                .ok_or_else(|| failure("awk-syntax-node-limit"))?;
            let parent = call
                .function_index
                .and_then(|index| u64::try_from(index).ok())
                .and_then(|index| index.checked_mul(3))
                .and_then(|offset| offset.checked_add(2))
                .unwrap_or(1);
            facts.push(SyntaxFact::new(
                local_id,
                Some(parent),
                SyntaxFactKind::Occurrence,
                source_span(file, call.start, call.end)?,
                if parent == 1 { 1 } else { 2 },
                label("awk.function.call"),
            ));
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
        let scoped_gap = !source.bytes().is_empty() && can_emit_gap(request, sink.staged_usage());
        if scoped_gap {
            let reference = source.source_ref();
            let gap_code = if parsed.lexical_incomplete {
                "awk-lexical-structure-partial"
            } else {
                "awk-expression-semantics-partial"
            };
            let diagnostic = AdapterDiagnostic::new(
                DiagnosticCode::new(gap_code).expect("fixed AWK diagnostic code"),
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
        let source_bytes = source.bytes().len();
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Bounded,
            source_bytes,
            source_bytes,
            usize::from(source_bytes > 0),
            vec![
                DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                DomainCoverage::new(
                    FactDomain::Entities,
                    CoverageStatus::Bounded,
                    parsed.functions.len(),
                    parsed.functions.len(),
                    0,
                )?,
                DomainCoverage::new(
                    FactDomain::Occurrences,
                    CoverageStatus::Bounded,
                    parsed.calls.len(),
                    parsed.calls.len(),
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
struct Function {
    start: usize,
    name_start: usize,
    name_end: usize,
    body_start: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct Call {
    start: usize,
    end: usize,
    function_index: Option<usize>,
}

struct ParsedSource {
    functions: Vec<Function>,
    calls: Vec<Call>,
    fact_count: usize,
    lexical_incomplete: bool,
}

fn scan_source(
    source: &[u8],
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<ParsedSource, AdapterError> {
    if max_syntax_nodes == 0 {
        return Err(failure("awk-syntax-node-limit"));
    }
    std::str::from_utf8(source).map_err(|_| failure("invalid-utf8"))?;
    let (masked, lexical_incomplete) = mask_non_code(source, cancellation)?;
    let mut functions = Vec::new();
    let mut cursor = 0;
    while cursor < masked.len() {
        if cursor % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        let byte = masked[cursor];
        if byte == b'{' {
            cursor =
                balanced_end(&masked, cursor, b'{', b'}', cancellation)?.unwrap_or(masked.len());
            continue;
        }
        if !identifier_start(byte) {
            cursor += 1;
            continue;
        }
        let end = identifier_end(&masked, cursor);
        if matches!(masked.get(cursor..end), Some(b"function" | b"func"))
            && let Some(function) = parse_function(&masked, cursor, end, cancellation)?
        {
            functions
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            functions.push(function);
            if functions
                .len()
                .checked_mul(3)
                .and_then(|count| count.checked_add(1))
                .is_none_or(|count| count > max_syntax_nodes)
            {
                return Err(failure("awk-syntax-node-limit"));
            }
            cursor = function.end;
        } else {
            cursor = end;
        }
    }
    let mut calls = Vec::new();
    cursor = 0;
    while cursor < masked.len() {
        if cursor % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        if !identifier_start(masked[cursor]) {
            cursor += 1;
            continue;
        }
        let end = identifier_end(&masked, cursor);
        let name = masked.get(cursor..end).unwrap_or_default();
        let definition_index = functions.partition_point(|function| function.name_start < cursor);
        let definition = functions
            .get(definition_index)
            .is_some_and(|function| function.name_start == cursor && function.name_end == end);
        let previous = cursor.checked_sub(1).and_then(|index| masked.get(index));
        if masked.get(end) == Some(&b'(')
            && !definition
            && !keyword(name)
            && !matches!(previous, Some(b'@' | b'$' | b':'))
        {
            let function_index = functions
                .partition_point(|function| function.body_start <= cursor)
                .checked_sub(1)
                .filter(|index| cursor < functions[*index].end);
            calls
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            calls.push(Call {
                start: cursor,
                end,
                function_index,
            });
            if functions
                .len()
                .checked_mul(3)
                .and_then(|count| count.checked_add(1))
                .and_then(|count| count.checked_add(calls.len()))
                .is_none_or(|count| count > max_syntax_nodes)
            {
                return Err(failure("awk-syntax-node-limit"));
            }
        }
        cursor = end;
    }
    cancellation.check()?;
    let fact_count = functions
        .len()
        .checked_mul(3)
        .and_then(|count| count.checked_add(1))
        .and_then(|count| count.checked_add(calls.len()))
        .ok_or_else(|| failure("awk-syntax-node-limit"))?;
    Ok(ParsedSource {
        functions,
        calls,
        fact_count,
        lexical_incomplete,
    })
}

fn mask_non_code(
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<(Vec<u8>, bool), AdapterError> {
    let mut masked = Vec::new();
    masked
        .try_reserve_exact(source.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    masked.extend_from_slice(source);
    let mut cursor = 0;
    let mut operand_before = false;
    let mut incomplete = false;
    while cursor < source.len() {
        if cursor % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        let byte = source[cursor];
        if byte == b'#' {
            let mut end = cursor;
            while end < source.len() && source[end] != b'\n' {
                if end % CHECK_INTERVAL == 0 {
                    cancellation.check()?;
                }
                end += 1;
            }
            mask_range(&mut masked, cursor, end);
            cursor = end;
            continue;
        }
        if identifier_start(byte) {
            let end = identifier_end(source, cursor);
            operand_before = !matches!(
                source.get(cursor..end),
                Some(b"return" | b"print" | b"printf" | b"else" | b"case")
            );
            cursor = end;
            continue;
        }
        if byte == b'"' || byte == b'/' && !operand_before {
            let delimiter = byte;
            let mut end = cursor + 1;
            let mut last_check = cursor;
            let mut in_class = false;
            let mut closed = false;
            while end < source.len() {
                if end - last_check >= CHECK_INTERVAL {
                    cancellation.check()?;
                    last_check = end;
                }
                let current = source[end];
                if current == b'\\' {
                    end = (end + 2).min(source.len());
                    continue;
                }
                if delimiter == b'/' {
                    if current == b'[' {
                        in_class = true;
                    } else if current == b']' {
                        in_class = false;
                    }
                }
                end += 1;
                if current == delimiter && !in_class {
                    closed = true;
                    break;
                }
            }
            incomplete |= !closed;
            mask_range(&mut masked, cursor, end);
            cursor = end;
            operand_before = closed;
            continue;
        }
        if matches!(byte, b'\n' | b'\r' | b';') {
            operand_before = false;
        } else if !byte.is_ascii_whitespace() {
            operand_before = identifier_continue(byte) || matches!(byte, b')' | b']' | b'}');
        }
        cursor += 1;
    }
    Ok((masked, incomplete))
}

fn mask_range(masked: &mut [u8], start: usize, end: usize) {
    for byte in &mut masked[start..end] {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
}

fn parse_function(
    masked: &[u8],
    start: usize,
    keyword_end: usize,
    cancellation: &Cancellation,
) -> Result<Option<Function>, AdapterError> {
    if !masked.get(keyword_end).is_some_and(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let name_start = skip_space(masked, keyword_end);
    if !masked
        .get(name_start)
        .is_some_and(|byte| identifier_start(*byte))
    {
        return Ok(None);
    }
    let name_end = identifier_end(masked, name_start);
    let open = skip_space(masked, name_end);
    if masked.get(open) != Some(&b'(') {
        return Ok(None);
    }
    let Some(parameter_end) = balanced_end(masked, open, b'(', b')', cancellation)? else {
        return Ok(None);
    };
    let body_start = skip_space(masked, parameter_end);
    if masked.get(body_start) != Some(&b'{') {
        return Ok(None);
    }
    let Some(end) = balanced_end(masked, body_start, b'{', b'}', cancellation)? else {
        return Ok(None);
    };
    Ok(Some(Function {
        start,
        name_start,
        name_end,
        body_start,
        end,
    }))
}

fn balanced_end(
    source: &[u8],
    start: usize,
    open: u8,
    close: u8,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let mut depth = 0_usize;
    for (index, byte) in source.iter().enumerate().skip(start) {
        if index % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        if *byte == open {
            let Some(next) = depth.checked_add(1) else {
                return Ok(None);
            };
            depth = next;
        } else if *byte == close {
            let Some(next) = depth.checked_sub(1) else {
                return Ok(None);
            };
            depth = next;
            if depth == 0 {
                return Ok(index.checked_add(1));
            }
        }
    }
    Ok(None)
}

fn skip_space(source: &[u8], mut cursor: usize) -> usize {
    while source.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}

fn identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn identifier_continue(byte: u8) -> bool {
    identifier_start(byte) || byte.is_ascii_digit()
}

fn identifier_end(source: &[u8], mut cursor: usize) -> usize {
    while source
        .get(cursor)
        .is_some_and(|byte| identifier_continue(*byte))
    {
        cursor += 1;
    }
    cursor
}

fn keyword(name: &[u8]) -> bool {
    matches!(
        name,
        b"function"
            | b"func"
            | b"if"
            | b"while"
            | b"for"
            | b"switch"
            | b"do"
            | b"return"
            | b"print"
            | b"printf"
            | b"getline"
            | b"delete"
            | b"exit"
            | b"next"
            | b"nextfile"
            | b"break"
            | b"continue"
    )
}

fn source_span(file: FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    let start = u64::try_from(start).map_err(|_| failure("awk-source-offset-limit"))?;
    let end = u64::try_from(end).map_err(|_| failure("awk-source-offset-limit"))?;
    SourceSpan::new(file, start, end).map_err(|_| failure("awk-source-span-invalid"))
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed AWK syntax label")
}

fn failure(value: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(value).expect("fixed AWK diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn functions_and_calls_keep_exact_source_spans() {
        let source = b"BEGIN { run() }\nfunction run(x) { return helper(x) }\nfunction helper(y) { return y }\n";
        let parsed = scan_source(source, 32, &Cancellation::new()).expect("AWK source parses");
        assert_eq!(parsed.functions.len(), 2);
        assert_eq!(parsed.calls.len(), 2);
        assert_eq!(parsed.fact_count, 9);
        assert_eq!(
            &source[parsed.functions[0].name_start..parsed.functions[0].name_end],
            b"run"
        );
        assert_eq!(&source[parsed.calls[0].start..parsed.calls[0].end], b"run");
        assert_eq!(parsed.calls[0].function_index, None);
        assert_eq!(
            &source[parsed.calls[1].start..parsed.calls[1].end],
            b"helper"
        );
        assert_eq!(parsed.calls[1].function_index, Some(0));
        assert!(!parsed.lexical_incomplete);
    }

    #[test]
    fn comments_literals_regex_and_dynamic_calls_do_not_invent_direct_calls() {
        let source = b"# phantom()\nBEGIN { text = \"ghost() # /fake()/\"; value = /hidden[(]/; @indirect(); actual() }\nfunction actual() { print \"ignored()\" }\n";
        let parsed = scan_source(source, 16, &Cancellation::new()).expect("bounded AWK source");
        assert_eq!(parsed.functions.len(), 1);
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(
            &source[parsed.calls[0].start..parsed.calls[0].end],
            b"actual"
        );
        assert!(scan_source(source, 4, &Cancellation::new()).is_err());
    }

    #[test]
    fn regex_after_expression_keywords_does_not_become_a_call() {
        let source = b"function inspect(value) { if (value) return /ghost()/; print /hidden()/; return real(value) }\nfunction real(value) { return value }\n";
        let parsed = scan_source(source, 24, &Cancellation::new()).expect("bounded AWK source");
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(&source[parsed.calls[0].start..parsed.calls[0].end], b"real");
    }

    #[test]
    fn malformed_and_non_utf8_sources_fail_closed_without_ghost_definitions() {
        let source = b"function open() { text = \"unterminated\nfunction ghost() {}";
        let parsed = scan_source(source, 16, &Cancellation::new()).expect("source is retained");
        assert!(parsed.functions.is_empty());
        assert!(parsed.lexical_incomplete);
        assert!(scan_source(b"function f() {}\xff", 16, &Cancellation::new()).is_err());
    }
}
