//! First-party Erlang source parser for module and function indicators.
//! Macro expansion, dynamic dispatch and expression semantics remain bounded.

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, CoverageReport, DescriptorError, DiagnosticCode,
    DomainCoverage, EncodingId, LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider,
    ParseReport, ParseRequest, ResourceUsage, SinkError, StreamEnd, StreamUsage, SyntaxFact,
    SyntaxFactBatch, SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel,
    canonical_erlang_function_head, canonical_erlang_qualified_function_call, erlang_atom_end,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId, content_hash};
use rootlight_ir::{
    AnalysisTier, CoverageStatus, DiagnosticSeverity, FactDomain, SourceRef, SourceSpan,
};

const CHECK_INTERVAL: usize = 4096;
const GAP_CODE_BYTES: usize = 48;
const GAP_ACCOUNTED_BYTES: usize = 256;

/// Bounded native provider for source-backed Erlang module and function facts.
pub struct ErlangParseProvider {
    capabilities: ParseCapabilities,
}

impl ErlangParseProvider {
    /// Returns the content identity of this first-party frontend.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        let parser = include_bytes!("erlang.rs");
        let names = include_bytes!("../../rootlight-adapter-sdk/src/erlang_names.rs");
        let mut source = Vec::with_capacity(parser.len() + names.len());
        source.extend_from_slice(parser);
        source.extend_from_slice(names);
        content_hash(&source)
    }

    /// Creates the parser with caller-owned source and syntax-node ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError`] when a ceiling is zero.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        let language = LanguageId::new("erlang").expect("fixed Erlang language label");
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

    /// Counts the same facts that parsing emits before admission.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] on invalid UTF-8, cancellation, or a syntax ceiling.
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
            request.limits().ir().max_string_bytes,
            cancellation,
        )?;
        Ok(parsed.fact_count)
    }
}

impl ParseProvider for ErlangParseProvider {
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
            request.limits().ir().max_string_bytes,
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
            label("erlang.source_file"),
        ));
        let mut next_id = 2_u64;
        let mut module_id = None;
        if let Some(module) = parsed.module {
            module_id = Some(next_id);
            facts.push(SyntaxFact::new(
                next_id,
                Some(1),
                SyntaxFactKind::Module,
                source.source_ref().span(),
                1,
                label("erlang.module.declaration"),
            ));
            facts.push(SyntaxFact::new(
                next_id + 1,
                Some(next_id),
                SyntaxFactKind::Occurrence,
                source_span(file, module.name_start, module.name_end)?,
                2,
                label("erlang.module.definition"),
            ));
            next_id += 2;
        }
        for function in &parsed.functions {
            cancellation.check()?;
            let parent = module_id.unwrap_or(1);
            let depth = if module_id.is_some() { 2 } else { 1 };
            facts.push(SyntaxFact::new(
                next_id,
                Some(parent),
                SyntaxFactKind::Declaration,
                source_span(file, function.start, function.end)?,
                depth,
                label("erlang.function.declaration"),
            ));
            facts.push(SyntaxFact::new(
                next_id + 1,
                Some(next_id),
                SyntaxFactKind::Signature,
                source_span(file, function.start, function.head_end)?,
                depth + 1,
                label("erlang.function.signature"),
            ));
            facts.push(SyntaxFact::new(
                next_id + 2,
                Some(next_id),
                SyntaxFactKind::Occurrence,
                source_span(file, function.start, function.head_end)?,
                depth + 1,
                label("erlang.function.definition"),
            ));
            next_id += 3;
        }
        let call_base = next_id;
        for (index, call) in parsed.calls.iter().enumerate() {
            cancellation.check()?;
            let local_id = call_base
                .checked_add(u64::try_from(index).map_err(|_| failure("erlang-syntax-node-limit"))?)
                .ok_or_else(|| failure("erlang-syntax-node-limit"))?;
            let parent = u64::try_from(call.function_index)
                .ok()
                .and_then(|index| index.checked_mul(3))
                .and_then(|offset| offset.checked_add(if module_id.is_some() { 4 } else { 2 }))
                .ok_or_else(|| failure("erlang-syntax-node-limit"))?;
            facts.push(SyntaxFact::new(
                local_id,
                Some(parent),
                SyntaxFactKind::Occurrence,
                source_span(file, call.start, call.end)?,
                if module_id.is_some() { 3 } else { 2 },
                label(if call.qualified {
                    "erlang.function.scoped_call"
                } else {
                    "erlang.function.call"
                }),
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
        let usage = sink.staged_usage();
        if !source.bytes().is_empty() && can_emit_gap(request, usage) {
            let reference = source.source_ref();
            let code = if parsed.lexical_incomplete {
                "erlang-lexical-structure-partial"
            } else {
                "erlang-macro-and-expression-semantics-partial"
            };
            let diagnostic = AdapterDiagnostic::new(
                DiagnosticCode::new(code).expect("fixed Erlang diagnostic code"),
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
        let entity_count = parsed.functions.len() + usize::from(parsed.module.is_some());
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
                    parsed.calls.len(),
                    parsed.calls.len(),
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

#[derive(Clone, Copy)]
struct Function {
    start: usize,
    head_end: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct Call {
    start: usize,
    end: usize,
    function_index: usize,
    qualified: bool,
}

struct ParsedSource {
    module: Option<Module>,
    functions: Vec<Function>,
    calls: Vec<Call>,
    fact_count: usize,
    lexical_incomplete: bool,
}

fn scan_source(
    source: &[u8],
    max_syntax_nodes: usize,
    max_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<ParsedSource, AdapterError> {
    if max_syntax_nodes == 0 {
        return Err(failure("erlang-syntax-node-limit"));
    }
    let text = std::str::from_utf8(source).map_err(|_| failure("invalid-utf8"))?;
    let (masked, mut lexical_incomplete) = mask_non_code(source, cancellation)?;
    let mut module = None;
    let mut functions = Vec::new();
    let mut form_start = 0;
    let (mut parens, mut brackets, mut braces, mut binaries) = (0_usize, 0_usize, 0_usize, 0_usize);
    let mut valid = true;
    let mut cursor = 0;
    while cursor < masked.len() {
        if cursor % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        let tail = &masked[cursor..];
        if tail.starts_with(b"<<") {
            binaries = binaries.saturating_add(1);
            cursor += 2;
            continue;
        }
        if tail.starts_with(b">>") && binaries > 0 {
            binaries -= 1;
            cursor += 2;
            continue;
        }
        match masked[cursor] {
            b'(' => parens = parens.saturating_add(1),
            b'[' => brackets = brackets.saturating_add(1),
            b'{' => braces = braces.saturating_add(1),
            b')' => valid &= decrement(&mut parens),
            b']' => valid &= decrement(&mut brackets),
            b'}' => valid &= decrement(&mut braces),
            b'.' if parens == 0
                && brackets == 0
                && braces == 0
                && binaries == 0
                && masked.get(cursor + 1).is_none_or(u8::is_ascii_whitespace) =>
            {
                if valid {
                    let start = skip_space(&masked, form_start);
                    if let Some(found) = parse_module(text, &masked, start, cursor + 1)
                        && module.is_none()
                    {
                        module = Some(found);
                    } else if let Some(function) =
                        parse_function(text, &masked, start, cursor + 1, max_name_bytes)
                    {
                        functions
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        functions.push(function);
                        if fact_count(module, functions.len(), 0)
                            .is_none_or(|n| n > max_syntax_nodes)
                        {
                            return Err(failure("erlang-syntax-node-limit"));
                        }
                    }
                } else {
                    lexical_incomplete = true;
                }
                form_start = cursor + 1;
                valid = true;
            }
            _ => {}
        }
        cursor += 1;
    }
    lexical_incomplete |= !valid || parens != 0 || brackets != 0 || braces != 0 || binaries != 0;
    lexical_incomplete |= masked
        .get(form_start..)
        .is_some_and(|tail| tail.iter().any(|byte| !byte.is_ascii_whitespace()));
    let mut calls = Vec::new();
    for (function_index, function) in functions.iter().enumerate() {
        let mut cursor = function.head_end;
        while cursor < function.end {
            if cursor % CHECK_INTERVAL == 0 {
                cancellation.check()?;
            }
            if !is_atom_start(&masked, cursor) {
                cursor += 1;
                continue;
            }
            let Some(name_end) = text
                .get(cursor..)
                .and_then(erlang_atom_end)
                .and_then(|n| cursor.checked_add(n))
            else {
                cursor += 1;
                continue;
            };
            let next = skip_space(&masked, name_end);
            let qualified = masked.get(next) == Some(&b':');
            let callee_end = if qualified {
                let callee_start = skip_space(&masked, next + 1);
                text.get(callee_start..)
                    .and_then(erlang_atom_end)
                    .and_then(|length| callee_start.checked_add(length))
            } else {
                Some(name_end)
            };
            let Some(callee_end) = callee_end else {
                cursor = name_end;
                continue;
            };
            let open = skip_space(&masked, callee_end);
            if masked.get(open) == Some(&b'(')
                && let Some(end) = balanced_end(&masked, open, b'(', b')')
                && end <= function.end
                && !is_qualified_or_macro(&masked, cursor)
                && if qualified {
                    canonical_erlang_qualified_function_call(&text[cursor..end], max_name_bytes)
                        .is_some()
                } else {
                    canonical_erlang_function_head(&text[cursor..end], max_name_bytes).is_some()
                }
                && !is_clause_head(&masked, cursor, end, function.end)
            {
                calls
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                calls.push(Call {
                    start: cursor,
                    end,
                    function_index,
                    qualified,
                });
                if fact_count(module, functions.len(), calls.len())
                    .is_none_or(|n| n > max_syntax_nodes)
                {
                    return Err(failure("erlang-syntax-node-limit"));
                }
            }
            cursor = name_end;
        }
    }
    cancellation.check()?;
    let fact_count = fact_count(module, functions.len(), calls.len())
        .ok_or_else(|| failure("erlang-syntax-node-limit"))?;
    Ok(ParsedSource {
        module,
        functions,
        calls,
        fact_count,
        lexical_incomplete,
    })
}

fn fact_count(module: Option<Module>, functions: usize, calls: usize) -> Option<usize> {
    functions
        .checked_mul(3)?
        .checked_add(1 + 2 * usize::from(module.is_some()))?
        .checked_add(calls)
}

fn parse_module(text: &str, masked: &[u8], start: usize, end: usize) -> Option<Module> {
    if !masked.get(start..)?.starts_with(b"-module") {
        return None;
    }
    let open = skip_space(masked, start + 7);
    if masked.get(open) != Some(&b'(') {
        return None;
    }
    let name_start = skip_space(masked, open + 1);
    let name_end = name_start.checked_add(erlang_atom_end(text.get(name_start..)?)?)?;
    let close = skip_space(masked, name_end);
    if masked.get(close) != Some(&b')') || skip_space(masked, close + 1) != end - 1 {
        return None;
    }
    Some(Module {
        name_start,
        name_end,
    })
}

fn parse_function(
    text: &str,
    masked: &[u8],
    start: usize,
    end: usize,
    max_name_bytes: usize,
) -> Option<Function> {
    if masked.get(start) == Some(&b'-') {
        return None;
    }
    let name_end = start.checked_add(erlang_atom_end(text.get(start..)?)?)?;
    let open = skip_space(masked, name_end);
    if masked.get(open) != Some(&b'(') {
        return None;
    }
    let head_end = balanced_end(masked, open, b'(', b')')?;
    if head_end >= end
        || !masked
            .get(head_end..end)?
            .windows(2)
            .any(|pair| pair == b"->")
    {
        return None;
    }
    canonical_erlang_function_head(text.get(start..head_end)?, max_name_bytes)?;
    Some(Function {
        start,
        head_end,
        end,
    })
}

fn is_clause_head(masked: &[u8], start: usize, end: usize, form_end: usize) -> bool {
    let previous = (0..start)
        .rev()
        .find(|&index| !masked[index].is_ascii_whitespace());
    if previous.is_none_or(|index| masked[index] != b';') {
        return false;
    }
    let next = masked.get(end..form_end).unwrap_or_default();
    next.windows(2)
        .position(|pair| pair == b"->")
        .is_some_and(|arrow| !next.get(..arrow).unwrap_or_default().contains(&b';'))
}

fn is_qualified_or_macro(masked: &[u8], start: usize) -> bool {
    (0..start)
        .rev()
        .find(|&index| !masked[index].is_ascii_whitespace())
        .is_some_and(|index| matches!(masked[index], b':' | b'?' | b'#' | b'.'))
}

fn is_atom_start(masked: &[u8], index: usize) -> bool {
    let byte = masked[index];
    if !(byte.is_ascii_lowercase() || byte == b'\'' || byte >= 0x80) {
        return false;
    }
    index == 0
        || !matches!(masked[index - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'@')
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
    let mut incomplete = false;
    let mut cursor = 0;
    if source.starts_with(b"\xef\xbb\xbf") {
        mask_range(&mut masked, 0, 3);
        cursor = 3;
    }
    while cursor < source.len() {
        if cursor % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        let byte = source[cursor];
        if byte == b'%' || cursor == 0 && source.starts_with(b"#!") {
            let end = source[cursor..]
                .iter()
                .position(|candidate| *candidate == b'\n')
                .map_or(source.len(), |length| cursor + length);
            mask_range(&mut masked, cursor, end);
            cursor = end;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            let mut end = cursor + 1;
            let mut closed = false;
            while end < source.len() {
                if end % CHECK_INTERVAL == 0 {
                    cancellation.check()?;
                }
                if source[end] == b'\\' {
                    end = (end + 2).min(source.len());
                } else if source[end] == byte {
                    end += 1;
                    closed = true;
                    break;
                } else {
                    end += 1;
                }
            }
            incomplete |= !closed;
            mask_range(&mut masked, cursor, end);
            if byte == b'\'' {
                masked[cursor] = b'\'';
            }
            cursor = end;
            continue;
        }
        if byte == b'$' {
            let end = (cursor
                + if source.get(cursor + 1) == Some(&b'\\') {
                    3
                } else {
                    2
                })
            .min(source.len());
            incomplete |= end == source.len() && end - cursor < 2;
            mask_range(&mut masked, cursor, end);
            cursor = end;
            continue;
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

fn balanced_end(source: &[u8], start: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, byte) in source.iter().enumerate().skip(start) {
        if *byte == open {
            depth = depth.checked_add(1)?;
        } else if *byte == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index + 1);
            }
        }
    }
    None
}

fn decrement(depth: &mut usize) -> bool {
    if let Some(next) = depth.checked_sub(1) {
        *depth = next;
        true
    } else {
        false
    }
}

fn skip_space(source: &[u8], mut cursor: usize) -> usize {
    while source.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}

fn source_span(file: FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    let start = u64::try_from(start).map_err(|_| failure("erlang-source-offset-limit"))?;
    let end = u64::try_from(end).map_err(|_| failure("erlang-source-offset-limit"))?;
    SourceSpan::new(file, start, end).map_err(|_| failure("erlang-source-span-invalid"))
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed Erlang syntax label")
}

fn failure(value: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(value).expect("fixed Erlang diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_scanner_keeps_module_overloads_and_clause_body_spans() {
        let source = b"-module(flow).\n-export([step/1, step/2]).\nstep(X) -> helper(X);\nstep(Y) when is_atom(Y) -> helper(Y).\nstep(X, Y) -> helper(X, Y).\n";
        let parsed = scan_source(source, 32, 128, &Cancellation::new()).expect("valid source");
        assert!(parsed.module.is_some());
        assert_eq!(parsed.functions.len(), 2);
        assert!(
            std::str::from_utf8(&source[parsed.functions[0].start..parsed.functions[0].end])
                .expect("valid UTF-8")
                .contains("step(Y)")
        );
        assert!(
            parsed
                .calls
                .iter()
                .any(|call| &source[call.start..call.end] == b"helper(X, Y)")
        );
        assert!(
            parsed
                .calls
                .iter()
                .all(|call| &source[call.start..call.end] != b"step(Y)")
        );
        assert_eq!(parsed.fact_count, 1 + 2 + 2 * 3 + parsed.calls.len());
    }

    #[test]
    fn comments_literals_macros_and_remote_calls_do_not_create_local_calls() {
        let source = b"-module('flow').\nrun(X) -> % ghost(X).\n  io:format(\"ghost(X).\"), ?MISSING(X), local(#{k => <<1,2>>}), 'quoted-call'(X).\n";
        let parsed = scan_source(source, 16, 128, &Cancellation::new()).expect("valid source");
        let calls = parsed
            .calls
            .iter()
            .map(|call| &source[call.start..call.end])
            .collect::<Vec<_>>();
        assert!(calls.contains(&b"local(#{k => <<1,2>>})".as_slice()));
        assert!(calls.contains(&b"'quoted-call'(X)".as_slice()));
        assert!(calls.contains(&b"io:format(\"ghost(X).\")".as_slice()));
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with(b"format(") || call.starts_with(b"MISSING("))
        );
    }

    #[test]
    fn static_remote_calls_keep_full_spans_without_dynamic_targets() {
        let source = b"-module(entry).\nrun(X, Module, Function) ->\n  remote:work(X), 'remote' : 'work'(X, []), remote % owner\n  : work % name\n  (X), Module:work(X), remote:Function(X), ?MODULE:work(X), remote:?FUNCTION(X), Record#state.target:work(X).\n";
        let parsed = scan_source(source, 32, 128, &Cancellation::new()).expect("bounded source");
        let remote = parsed
            .calls
            .iter()
            .filter(|call| call.qualified)
            .collect::<Vec<_>>();
        assert_eq!(
            remote.len(),
            3,
            "captured calls: {:?}",
            remote
                .iter()
                .map(|call| &source[call.start..call.end])
                .collect::<Vec<_>>()
        );
        for call in remote {
            assert_eq!(call.function_index, 0);
            assert!(
                canonical_erlang_qualified_function_call(
                    std::str::from_utf8(&source[call.start..call.end]).expect("UTF-8"),
                    128
                )
                .is_some()
            );
        }
        assert_eq!(parsed.fact_count, 1 + 2 + 3 + parsed.calls.len());
        assert!(scan_source(source, parsed.fact_count - 1, 128, &Cancellation::new()).is_err());
    }

    #[test]
    fn invalid_or_unclosed_source_does_not_invent_entities() {
        let parsed = scan_source(
            b"-spec ghost(X) -> X.\nrun(X) -> ok.\nunfinished(X",
            16,
            128,
            &Cancellation::new(),
        )
        .expect("bounded source");
        assert_eq!(parsed.functions.len(), 1);
        assert!(parsed.lexical_incomplete);
        assert!(scan_source(b"run() -> ok.\n", 3, 128, &Cancellation::new()).is_err());
    }

    #[test]
    fn frontend_digest_includes_the_shared_arity_rules() {
        assert_ne!(
            ErlangParseProvider::binary_digest(),
            content_hash(include_bytes!("erlang.rs"))
        );
    }
}
