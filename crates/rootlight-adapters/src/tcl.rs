//! First-party Tcl scanner for literal procedures, formal parameters and namespaces.
//! It retains exact source spans without executing Tcl substitutions; dynamic
//! command construction and unvisited script-bearing arguments stay bounded.

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
const MAX_SCRIPT_DEPTH: usize = 64;
const MAX_NAME_BYTES: usize = 256;
const GAP_CODE_BYTES: usize = 48;
const GAP_ACCOUNTED_BYTES: usize = 256;

/// Bounded parser for source-written Tcl procedures, parameters and namespaces.
pub struct TclParseProvider {
    capabilities: ParseCapabilities,
}

impl TclParseProvider {
    /// Returns the content identity of this first-party frontend.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("tcl.rs"))
    }

    /// Creates a parser with caller-owned source and syntax-node ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError`] when a ceiling is invalid.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed Tcl language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        let language = LanguageId::new("tcl").expect("fixed Tcl language label");
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

    /// Counts emitted facts before stream admission.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for invalid UTF-8, cancellation, or a syntax ceiling.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        Ok(scan_source(
            request.source().bytes(),
            request
                .limits()
                .max_syntax_nodes()
                .min(self.capabilities.max_syntax_nodes()),
            cancellation,
        )?
        .fact_count)
    }
}

impl ParseProvider for TclParseProvider {
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
            label("tcl.source_file"),
        ));
        let mut ids = Vec::new();
        let mut depths = Vec::new();
        ids.try_reserve_exact(parsed.items.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        depths
            .try_reserve_exact(parsed.items.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut next_id = 2_u64;
        for item in &parsed.items {
            cancellation.check()?;
            let (parent, depth) = if let Some(index) = item.parent_item {
                (
                    *ids.get(index)
                        .ok_or_else(|| failure("tcl-parent-order-invalid"))?,
                    depths
                        .get(index)
                        .ok_or_else(|| failure("tcl-parent-order-invalid"))?
                        + 1,
                )
            } else {
                (1, 1)
            };
            let declaration_id = next_id;
            ids.push(declaration_id);
            depths.push(depth);
            let declaration = match item.kind {
                ItemKind::Procedure => "tcl.function.declaration",
                ItemKind::Namespace => "tcl.namespace.declaration",
            };
            facts.push(SyntaxFact::new(
                next_id,
                Some(parent),
                SyntaxFactKind::Declaration,
                source_span(file, item.start, item.end)?,
                depth,
                label(declaration),
            ));
            if item.kind == ItemKind::Procedure {
                facts.push(SyntaxFact::new(
                    next_id + 1,
                    Some(next_id),
                    SyntaxFactKind::Signature,
                    source_span(file, item.start, item.body_start)?,
                    depth + 1,
                    label("tcl.function.signature"),
                ));
                next_id += 1;
            }
            facts.push(SyntaxFact::new(
                next_id + 1,
                Some(declaration_id),
                SyntaxFactKind::Occurrence,
                source_span(file, item.name_start, item.name_end)?,
                depth + 1,
                label(match item.kind {
                    ItemKind::Procedure => "tcl.function.definition",
                    ItemKind::Namespace => "tcl.namespace.definition",
                }),
            ));
            next_id += 2;
        }
        for parameter in &parsed.parameters {
            cancellation.check()?;
            let parent = *ids
                .get(parameter.owner_item)
                .ok_or_else(|| failure("tcl-parent-order-invalid"))?;
            let depth = depths
                .get(parameter.owner_item)
                .ok_or_else(|| failure("tcl-parent-order-invalid"))?
                + 1;
            facts.push(SyntaxFact::new(
                next_id,
                Some(parent),
                SyntaxFactKind::Declaration,
                source_span(file, parameter.start, parameter.end)?,
                depth,
                label("tcl.parameter.declaration"),
            ));
            facts.push(SyntaxFact::new(
                next_id + 1,
                Some(next_id),
                SyntaxFactKind::Occurrence,
                source_span(file, parameter.name_start, parameter.name_end)?,
                depth + 1,
                label("tcl.parameter.definition"),
            ));
            next_id += 2;
        }
        for call in &parsed.calls {
            cancellation.check()?;
            let (parent, depth) = if let Some(index) = call.owner_item {
                (
                    *ids.get(index)
                        .ok_or_else(|| failure("tcl-parent-order-invalid"))?,
                    depths
                        .get(index)
                        .ok_or_else(|| failure("tcl-parent-order-invalid"))?
                        + 1,
                )
            } else {
                (1, 1)
            };
            facts.push(SyntaxFact::new(
                next_id,
                Some(parent),
                SyntaxFactKind::Occurrence,
                source_span(file, call.start, call.end)?,
                depth,
                label("tcl.command.call"),
            ));
            next_id += 1;
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
        let mut diagnostic_count = 0_usize;
        for gap in &parsed.formal_gaps {
            cancellation.check()?;
            if !can_emit_gap(request, sink.staged_usage()) {
                return Err(failure("tcl-formal-gap-report-limit"));
            }
            let reference = source.source_ref();
            let diagnostic = AdapterDiagnostic::new(
                DiagnosticCode::new(gap.code).expect("fixed Tcl formal diagnostic code"),
                DiagnosticSeverity::Warning,
                Some(SourceRef::new(
                    reference.repository(),
                    reference.generation(),
                    source_span(file, gap.start, gap.end)?,
                    reference.content_hash(),
                    None,
                )),
                CoverageStatus::Bounded,
            );
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), Vec::new(), vec![diagnostic]),
                cancellation,
            )?;
            diagnostic_count += 1;
        }
        if !source.bytes().is_empty() && can_emit_gap(request, sink.staged_usage()) {
            let reference = source.source_ref();
            let code = if parsed.lexical_incomplete {
                "tcl-lexical-structure-partial"
            } else {
                "tcl-dynamic-command-semantics-partial"
            };
            let diagnostic = AdapterDiagnostic::new(
                DiagnosticCode::new(code).expect("fixed Tcl diagnostic code"),
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
            diagnostic_count += 1;
        }
        let usage = sink.staged_usage();
        let bytes = source.bytes().len();
        let entities = parsed
            .items
            .len()
            .checked_add(parsed.parameters.len())
            .ok_or_else(|| failure("tcl-syntax-node-limit"))?;
        let occurrences = entities
            .checked_add(parsed.calls.len())
            .ok_or_else(|| failure("tcl-syntax-node-limit"))?;
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Bounded,
            bytes,
            bytes,
            diagnostic_count,
            vec![
                DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                DomainCoverage::new(
                    FactDomain::Entities,
                    CoverageStatus::Bounded,
                    entities,
                    entities,
                    0,
                )?,
                DomainCoverage::new(
                    FactDomain::Occurrences,
                    CoverageStatus::Bounded,
                    occurrences,
                    occurrences,
                    0,
                )?,
                DomainCoverage::new(
                    FactDomain::Diagnostics,
                    CoverageStatus::Bounded,
                    diagnostic_count,
                    diagnostic_count,
                    0,
                )?,
            ],
        )?;
        let maximum_depth = facts.iter().map(SyntaxFact::depth).max().unwrap_or(0);
        Ok(ParseReport::new(
            coverage,
            ResourceUsage::new(bytes, facts.len(), facts.len(), maximum_depth, None, usage),
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Procedure,
    Namespace,
}

#[derive(Clone, Copy)]
struct Item {
    kind: ItemKind,
    start: usize,
    end: usize,
    body_start: usize,
    name_start: usize,
    name_end: usize,
    parent_item: Option<usize>,
}

#[derive(Clone, Copy)]
struct Call {
    start: usize,
    end: usize,
    owner_item: Option<usize>,
}

#[derive(Clone, Copy)]
struct Parameter {
    start: usize,
    end: usize,
    name_start: usize,
    name_end: usize,
    owner_item: usize,
}

struct FormalGap {
    start: usize,
    end: usize,
    code: &'static str,
}

struct FormalList {
    parameters: Vec<Parameter>,
    gaps: Vec<FormalGap>,
}

struct ParsedSource {
    items: Vec<Item>,
    parameters: Vec<Parameter>,
    formal_gaps: Vec<FormalGap>,
    calls: Vec<Call>,
    fact_count: usize,
    lexical_incomplete: bool,
}

#[derive(Clone, Copy)]
struct Script {
    start: usize,
    end: usize,
    owner_item: Option<usize>,
    depth: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WordForm {
    Bare,
    Braced,
    Quoted,
}

#[derive(Clone, Copy)]
struct Word {
    start: usize,
    end: usize,
    content_start: usize,
    content_end: usize,
    form: WordForm,
}

impl Word {
    fn literal_name(self, source: &[u8]) -> Option<&[u8]> {
        if self.form == WordForm::Quoted {
            return None;
        }
        let bytes = source.get(self.content_start..self.content_end)?;
        let text = std::str::from_utf8(bytes).ok()?;
        (!bytes.is_empty()
            && bytes.len() <= MAX_NAME_BYTES
            && text.chars().all(|character| {
                !character.is_control()
                    && !character.is_whitespace()
                    && !matches!(character, '\\' | '$' | '[' | ']' | '{' | '}' | ';' | '"')
            }))
        .then_some(bytes)
    }

    fn braced_script(self) -> Option<(usize, usize)> {
        (self.form == WordForm::Braced).then_some((self.content_start, self.content_end))
    }
}

fn scan_source(
    source: &[u8],
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<ParsedSource, AdapterError> {
    if max_syntax_nodes == 0 {
        return Err(failure("tcl-syntax-node-limit"));
    }
    std::str::from_utf8(source).map_err(|_| failure("invalid-utf8"))?;
    let mut scripts = vec![Script {
        start: 0,
        end: source.len(),
        owner_item: None,
        depth: 0,
    }];
    let mut items = Vec::new();
    let mut command_calls = Vec::new();
    let mut parameters: Vec<Parameter> = Vec::new();
    let mut formal_gaps: Vec<FormalGap> = Vec::new();
    let mut lexical_incomplete = false;
    let mut script_index = 0;
    while let Some(script) = scripts.get(script_index).copied() {
        script_index += 1;
        let mut cursor = script.start;
        while cursor < script.end {
            if cursor.is_multiple_of(CHECK_INTERVAL) {
                cancellation.check()?;
            }
            loop {
                skip_word_space(source, &mut cursor, script.end);
                if cursor < script.end
                    && source
                        .get(cursor)
                        .is_some_and(|byte| matches!(byte, b'\r' | b'\n' | b';'))
                {
                    cursor = line_ending_end(source, cursor, script.end).unwrap_or(cursor + 1);
                } else {
                    break;
                }
            }
            if cursor >= script.end {
                break;
            }
            if source.get(cursor) == Some(&b'#') {
                cursor = skip_comment(source, cursor, script.end);
                continue;
            }
            let command_start = cursor;
            let mut words = [None; 5];
            let mut word_count = 0_usize;
            let mut last_word = None;
            loop {
                if word_count.is_multiple_of(CHECK_INTERVAL) {
                    cancellation.check()?;
                }
                skip_word_space(source, &mut cursor, script.end);
                if cursor >= script.end
                    || source
                        .get(cursor)
                        .is_some_and(|byte| matches!(byte, b'\r' | b'\n' | b';'))
                {
                    break;
                }
                let Some(word) = read_word(source, cursor, script.end, cancellation)? else {
                    lexical_incomplete = true;
                    cursor = script.end;
                    break;
                };
                if let Some(slot) = words.get_mut(word_count) {
                    *slot = Some(word);
                }
                word_count = word_count.saturating_add(1);
                last_word = Some(word);
                queue_substitutions(
                    source,
                    word,
                    &mut scripts,
                    script.owner_item,
                    script.depth,
                    max_syntax_nodes,
                    cancellation,
                )?;
                cursor = word.end;
            }
            let command_end = cursor;
            let Some(first) = words[0] else {
                continue;
            };
            if word_count == 4
                && first.literal_name(source) == Some(&b"proc"[..])
                && let (Some(name), Some(args), Some(body)) = (words[1], words[2], words[3])
                && name.literal_name(source).is_some()
                && static_formal_list(args, source)
                && let Some((body_start, body_end)) = body.braced_script()
            {
                let index = items.len();
                reserve_item(&mut items, max_syntax_nodes)?;
                items.push(Item {
                    kind: ItemKind::Procedure,
                    start: command_start,
                    end: command_end,
                    body_start: body.start,
                    name_start: name.content_start,
                    name_end: name.content_end,
                    parent_item: script.owner_item,
                });
                let formals = scan_formals(source, args, index, max_syntax_nodes, cancellation)?;
                let parameter_count = parameters
                    .len()
                    .checked_add(formals.parameters.len())
                    .ok_or_else(|| failure("tcl-syntax-node-limit"))?;
                let gap_count = formal_gaps
                    .len()
                    .checked_add(formals.gaps.len())
                    .ok_or_else(|| failure("tcl-syntax-node-limit"))?;
                if parameter_count.saturating_mul(2).saturating_add(gap_count) > max_syntax_nodes {
                    return Err(failure("tcl-syntax-node-limit"));
                }
                parameters
                    .try_reserve(formals.parameters.len())
                    .map_err(|_| SinkError::AllocationFailed)?;
                formal_gaps
                    .try_reserve(formals.gaps.len())
                    .map_err(|_| SinkError::AllocationFailed)?;
                parameters.extend(formals.parameters);
                formal_gaps.extend(formals.gaps);
                push_script(
                    &mut scripts,
                    body_start,
                    body_end,
                    Some(index),
                    script.depth,
                    max_syntax_nodes,
                )?;
            } else if word_count == 4
                && first.literal_name(source) == Some(&b"namespace"[..])
                && words[1].and_then(|word| word.literal_name(source)) == Some(&b"eval"[..])
                && let (Some(name), Some(body)) = (words[2], words[3])
                && name.literal_name(source).is_some()
                && let Some((body_start, body_end)) = body.braced_script()
            {
                let index = items.len();
                reserve_item(&mut items, max_syntax_nodes)?;
                items.push(Item {
                    kind: ItemKind::Namespace,
                    start: command_start,
                    end: command_end,
                    body_start: body.start,
                    name_start: name.content_start,
                    name_end: name.content_end,
                    parent_item: script.owner_item,
                });
                push_script(
                    &mut scripts,
                    body_start,
                    body_end,
                    Some(index),
                    script.depth,
                    max_syntax_nodes,
                )?;
            } else if first.literal_name(source).is_some() {
                if command_calls.len() >= max_syntax_nodes {
                    return Err(failure("tcl-syntax-node-limit"));
                }
                command_calls
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                command_calls.push(Call {
                    start: first.content_start,
                    end: first.content_end,
                    owner_item: script.owner_item,
                });
            }
            queue_script_arguments(
                source,
                first,
                &words,
                word_count,
                last_word,
                command_end,
                &mut scripts,
                script.owner_item,
                script.depth,
                max_syntax_nodes,
                cancellation,
            )?;
        }
    }
    let declaration_facts = items.iter().try_fold(0_usize, |count, item| {
        count.checked_add(if item.kind == ItemKind::Procedure {
            3
        } else {
            2
        })
    });
    let fact_count = declaration_facts
        .and_then(|count| {
            parameters
                .len()
                .checked_mul(2)
                .and_then(|extra| count.checked_add(extra))
        })
        .and_then(|count| count.checked_add(command_calls.len()))
        .and_then(|count| count.checked_add(1))
        .filter(|count| *count <= max_syntax_nodes)
        .ok_or_else(|| failure("tcl-syntax-node-limit"))?;
    cancellation.check()?;
    Ok(ParsedSource {
        items,
        parameters,
        formal_gaps,
        calls: command_calls,
        fact_count,
        lexical_incomplete,
    })
}

fn static_formal_list(word: Word, source: &[u8]) -> bool {
    word.form == WordForm::Braced
        || source
            .get(word.content_start..word.content_end)
            .is_some_and(|bytes| !bytes.iter().any(|byte| matches!(byte, b'\\' | b'$' | b'[')))
}

fn scan_formals(
    source: &[u8],
    args: Word,
    owner_item: usize,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<FormalList, AdapterError> {
    let unsupported = |code| FormalList {
        parameters: Vec::new(),
        gaps: vec![FormalGap {
            start: args.start,
            end: args.end,
            code,
        }],
    };
    for offset in args.content_start..args.content_end {
        if offset.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        // The command's pre-pass changes this text before either list layer.
        if line_continuation_end(source, offset, args.content_end).is_some() {
            return Ok(unsupported("tcl-formal-escape-not-analyzed"));
        }
    }
    let mut result = FormalList {
        parameters: Vec::new(),
        gaps: Vec::new(),
    };
    let mut cursor = args.content_start;
    loop {
        skip_list_space(source, &mut cursor, args.content_end, cancellation)?;
        if cursor >= args.content_end {
            return Ok(result);
        }
        let Some(specifier) = read_list_word(source, cursor, args.content_end, cancellation)?
        else {
            return Ok(unsupported("tcl-formal-list-not-analyzed"));
        };
        cursor = specifier.end;
        let specifier_text = source
            .get(specifier.content_start..specifier.content_end)
            .ok_or_else(|| failure("tcl-source-span-invalid"))?;
        if specifier.form != WordForm::Braced && specifier_text.contains(&b'\\') {
            // Decoding the outer list could change the inner list's boundaries.
            return Ok(unsupported("tcl-formal-escape-not-analyzed"));
        }
        let mut field = specifier.content_start;
        skip_list_space(source, &mut field, specifier.content_end, cancellation)?;
        let Some(name) = read_list_word(source, field, specifier.content_end, cancellation)? else {
            return Ok(unsupported("tcl-formal-list-not-analyzed"));
        };
        field = name.end;
        skip_list_space(source, &mut field, specifier.content_end, cancellation)?;
        if field < specifier.content_end {
            let Some(default) = read_list_word(source, field, specifier.content_end, cancellation)?
            else {
                return Ok(unsupported("tcl-formal-list-not-analyzed"));
            };
            field = default.end;
            skip_list_space(source, &mut field, specifier.content_end, cancellation)?;
        }
        if field != specifier.content_end {
            return Ok(unsupported("tcl-formal-list-not-analyzed"));
        }
        let literal = Word {
            form: WordForm::Bare,
            ..name
        }
        .literal_name(source);
        let additional = if literal.is_some() { 2 } else { 1 };
        if result
            .parameters
            .len()
            .saturating_mul(2)
            .saturating_add(result.gaps.len())
            .saturating_add(additional)
            > maximum
        {
            return Err(failure("tcl-syntax-node-limit"));
        }
        if literal.is_some() {
            result
                .parameters
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.parameters.push(Parameter {
                start: specifier.start,
                end: specifier.end,
                name_start: name.content_start,
                name_end: name.content_end,
                owner_item,
            });
        } else {
            result
                .gaps
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.gaps.push(FormalGap {
                start: name.start,
                end: name.end,
                code: "tcl-formal-name-not-analyzed",
            });
        }
    }
}

fn list_separator(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn skip_list_space(
    source: &[u8],
    cursor: &mut usize,
    end: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    while *cursor < end
        && source
            .get(*cursor)
            .is_some_and(|byte| list_separator(*byte))
    {
        if (*cursor).is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        *cursor += 1;
    }
    Ok(())
}

fn read_list_word(
    source: &[u8],
    start: usize,
    end: usize,
    cancellation: &Cancellation,
) -> Result<Option<Word>, AdapterError> {
    let Some(first) = source
        .get(start)
        .copied()
        .filter(|byte| !list_separator(*byte))
    else {
        return Ok(None);
    };
    if start >= end {
        return Ok(None);
    }
    if first == b'{' {
        let Some(after) = balanced_braces(source, start, end, cancellation)? else {
            return Ok(None);
        };
        if after < end && !source.get(after).is_some_and(|byte| list_separator(*byte)) {
            return Ok(None);
        }
        return Ok(Some(Word {
            start,
            end: after,
            content_start: start + 1,
            content_end: after - 1,
            form: WordForm::Braced,
        }));
    }
    let quoted = first == b'"';
    let mut cursor = start + usize::from(quoted);
    while cursor < end {
        if cursor.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let Some(byte) = source.get(cursor).copied() else {
            return Ok(None);
        };
        if byte == b'\\' {
            cursor = cursor.saturating_add(2).min(end);
            continue;
        }
        if quoted && byte == b'"' {
            let after = cursor + 1;
            if after < end && !source.get(after).is_some_and(|byte| list_separator(*byte)) {
                return Ok(None);
            }
            return Ok(Some(Word {
                start,
                end: after,
                content_start: start + 1,
                content_end: cursor,
                form: WordForm::Quoted,
            }));
        }
        if !quoted && list_separator(byte) {
            break;
        }
        cursor += 1;
    }
    Ok((!quoted).then_some(Word {
        start,
        end: cursor,
        content_start: start,
        content_end: cursor,
        form: WordForm::Bare,
    }))
}

fn reserve_item(items: &mut Vec<Item>, max_syntax_nodes: usize) -> Result<(), AdapterError> {
    if items.len().saturating_mul(2).saturating_add(3) > max_syntax_nodes {
        return Err(failure("tcl-syntax-node-limit"));
    }
    items
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    Ok(())
}

fn push_script(
    scripts: &mut Vec<Script>,
    start: usize,
    end: usize,
    owner_item: Option<usize>,
    parent_depth: usize,
    max_syntax_nodes: usize,
) -> Result<(), AdapterError> {
    if scripts.len() >= max_syntax_nodes {
        return Err(failure("tcl-syntax-node-limit"));
    }
    let depth = parent_depth
        .checked_add(1)
        .filter(|depth| *depth <= MAX_SCRIPT_DEPTH)
        .ok_or_else(|| failure("tcl-script-depth-limit"))?;
    scripts
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    scripts.push(Script {
        start,
        end,
        owner_item,
        depth,
    });
    Ok(())
}

fn queue_substitutions(
    source: &[u8],
    word: Word,
    scripts: &mut Vec<Script>,
    owner_item: Option<usize>,
    parent_depth: usize,
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    if word.form == WordForm::Braced {
        return Ok(());
    }
    let mut cursor = word.content_start;
    while cursor < word.content_end {
        if cursor.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        match source.get(cursor) {
            Some(b'\\') => {
                cursor = line_continuation_end(source, cursor, word.content_end)
                    .unwrap_or((cursor + 2).min(word.content_end));
            }
            Some(b'[') => {
                let after = balanced_brackets(source, cursor, word.content_end, cancellation)?
                    .ok_or_else(|| failure("tcl-lexical-balance-invalid"))?;
                push_script(
                    scripts,
                    cursor + 1,
                    after - 1,
                    owner_item,
                    parent_depth,
                    max_syntax_nodes,
                )?;
                cursor = after;
            }
            Some(_) => cursor += 1,
            None => return Err(failure("tcl-lexical-balance-invalid")),
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the bounded source scan passes its existing queue context without allocating"
)]
fn queue_script_arguments(
    source: &[u8],
    first: Word,
    words: &[Option<Word>; 5],
    word_count: usize,
    last_word: Option<Word>,
    command_end: usize,
    scripts: &mut Vec<Script>,
    owner_item: Option<usize>,
    parent_depth: usize,
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let Some(name) = first.literal_name(source) else {
        return Ok(());
    };
    match name {
        b"if" => queue_if_scripts(
            source,
            first.end,
            command_end,
            scripts,
            owner_item,
            parent_depth,
            max_syntax_nodes,
            cancellation,
        )?,
        b"while" if word_count == 3 => {
            queue_braced_script(
                words[2],
                scripts,
                owner_item,
                parent_depth,
                max_syntax_nodes,
            )?;
        }
        b"for" if word_count == 5 => {
            for word in [words[1], words[3], words[4]] {
                queue_braced_script(word, scripts, owner_item, parent_depth, max_syntax_nodes)?;
            }
        }
        b"foreach" if word_count >= 4 && word_count.is_multiple_of(2) => {
            queue_braced_script(
                last_word,
                scripts,
                owner_item,
                parent_depth,
                max_syntax_nodes,
            )?;
        }
        b"catch" if (2..=4).contains(&word_count) => {
            queue_braced_script(
                words[1],
                scripts,
                owner_item,
                parent_depth,
                max_syntax_nodes,
            )?;
        }
        b"eval" if word_count == 2 => {
            queue_braced_script(
                words[1],
                scripts,
                owner_item,
                parent_depth,
                max_syntax_nodes,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn queue_braced_script(
    word: Option<Word>,
    scripts: &mut Vec<Script>,
    owner_item: Option<usize>,
    parent_depth: usize,
    max_syntax_nodes: usize,
) -> Result<(), AdapterError> {
    if let Some((start, end)) = word.and_then(Word::braced_script) {
        push_script(
            scripts,
            start,
            end,
            owner_item,
            parent_depth,
            max_syntax_nodes,
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum IfArgument {
    Condition,
    OptionalThen,
    Body,
    Clause,
    ElseBody,
    Done,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the variable-length if clauses are rescanned without retaining an argument vector"
)]
fn queue_if_scripts(
    source: &[u8],
    start: usize,
    end: usize,
    scripts: &mut Vec<Script>,
    owner_item: Option<usize>,
    parent_depth: usize,
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut cursor = start;
    let mut state = IfArgument::Condition;
    let mut word_count = 0_usize;
    while cursor < end {
        if word_count.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        skip_word_space(source, &mut cursor, end);
        if cursor >= end {
            break;
        }
        let Some(word) = read_word(source, cursor, end, cancellation)? else {
            break;
        };
        word_count = word_count.saturating_add(1);
        cursor = word.end;
        state = match state {
            IfArgument::Condition => IfArgument::OptionalThen,
            IfArgument::OptionalThen if word.literal_name(source) == Some(&b"then"[..]) => {
                IfArgument::Body
            }
            IfArgument::OptionalThen | IfArgument::Body => {
                queue_braced_script(
                    Some(word),
                    scripts,
                    owner_item,
                    parent_depth,
                    max_syntax_nodes,
                )?;
                IfArgument::Clause
            }
            IfArgument::Clause if word.literal_name(source) == Some(&b"elseif"[..]) => {
                IfArgument::Condition
            }
            IfArgument::Clause if word.literal_name(source) == Some(&b"else"[..]) => {
                IfArgument::ElseBody
            }
            IfArgument::Clause | IfArgument::ElseBody => {
                queue_braced_script(
                    Some(word),
                    scripts,
                    owner_item,
                    parent_depth,
                    max_syntax_nodes,
                )?;
                IfArgument::Done
            }
            IfArgument::Done => IfArgument::Done,
        };
    }
    Ok(())
}

fn read_word(
    source: &[u8],
    start: usize,
    end: usize,
    cancellation: &Cancellation,
) -> Result<Option<Word>, AdapterError> {
    let Some(&first) = source.get(start) else {
        return Ok(None);
    };
    if first == b'{' {
        let Some(after) = balanced_braces(source, start, end, cancellation)? else {
            return Ok(None);
        };
        if after < end
            && !source.get(after).is_some_and(|byte| word_separator(*byte))
            && line_continuation_end(source, after, end).is_none()
        {
            return Ok(None);
        }
        return Ok(Some(Word {
            start,
            end: after,
            content_start: start + 1,
            content_end: after - 1,
            form: WordForm::Braced,
        }));
    }
    if first == b'"' {
        let mut cursor = start + 1;
        while cursor < end {
            if cursor.is_multiple_of(CHECK_INTERVAL) {
                cancellation.check()?;
            }
            match source.get(cursor) {
                Some(b'\\') => {
                    cursor =
                        line_continuation_end(source, cursor, end).unwrap_or((cursor + 2).min(end));
                }
                Some(b'[') => {
                    let Some(after) = balanced_brackets(source, cursor, end, cancellation)? else {
                        return Ok(None);
                    };
                    cursor = after;
                }
                Some(b'"') => {
                    if cursor + 1 < end
                        && !source
                            .get(cursor + 1)
                            .is_some_and(|byte| word_separator(*byte))
                        && line_continuation_end(source, cursor + 1, end).is_none()
                    {
                        return Ok(None);
                    }
                    return Ok(Some(Word {
                        start,
                        end: cursor + 1,
                        content_start: start + 1,
                        content_end: cursor,
                        form: WordForm::Quoted,
                    }));
                }
                Some(_) => cursor += 1,
                None => return Ok(None),
            }
        }
        return Ok(None);
    }
    let mut cursor = start;
    while cursor < end {
        if cursor.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        match source.get(cursor) {
            Some(byte) if word_separator(*byte) => break,
            Some(b'\\') if line_continuation_end(source, cursor, end).is_some() => break,
            Some(b'\\') => cursor = (cursor + 2).min(end),
            Some(b'[') => {
                let Some(after) = balanced_brackets(source, cursor, end, cancellation)? else {
                    return Ok(None);
                };
                cursor = after;
            }
            Some(_) => cursor += 1,
            None => return Ok(None),
        }
    }
    Ok(Some(Word {
        start,
        end: cursor,
        content_start: start,
        content_end: cursor,
        form: WordForm::Bare,
    }))
}

fn word_separator(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b';')
}

fn line_ending_end(source: &[u8], cursor: usize, end: usize) -> Option<usize> {
    if cursor >= end {
        return None;
    }
    match source.get(cursor) {
        Some(b'\n') => Some(cursor + 1),
        Some(b'\r') if cursor + 1 < end && source.get(cursor + 1) == Some(&b'\n') => {
            Some(cursor + 2)
        }
        Some(b'\r') => Some(cursor + 1),
        _ => None,
    }
}

fn line_continuation_end(source: &[u8], cursor: usize, end: usize) -> Option<usize> {
    if source.get(cursor) != Some(&b'\\') {
        return None;
    }
    let mut after = line_ending_end(source, cursor + 1, end)?;
    while after < end
        && source
            .get(after)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        after += 1;
    }
    Some(after)
}

fn skip_word_space(source: &[u8], cursor: &mut usize, end: usize) {
    while *cursor < end {
        if source
            .get(*cursor)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            *cursor += 1;
        } else if let Some(after) = line_continuation_end(source, *cursor, end) {
            *cursor = after;
        } else {
            break;
        }
    }
}

fn skip_comment(source: &[u8], mut cursor: usize, end: usize) -> usize {
    while cursor < end {
        if let Some(after) = line_continuation_end(source, cursor, end) {
            cursor = after;
        } else if let Some(after) = line_ending_end(source, cursor, end) {
            return after;
        } else {
            cursor += 1;
        }
    }
    cursor
}

fn balanced_braces(
    source: &[u8],
    start: usize,
    end: usize,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let mut depth = 0_usize;
    let mut cursor = start;
    while cursor < end {
        if cursor.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let Some(&byte) = source.get(cursor) else {
            return Ok(None);
        };
        if byte == b'\\' {
            // Escaped braces do not change Tcl's brace nesting.
            cursor = line_continuation_end(source, cursor, end).unwrap_or((cursor + 2).min(end));
            continue;
        }
        if byte == b'{' {
            depth = depth
                .checked_add(1)
                .ok_or_else(|| failure("tcl-script-depth-limit"))?;
        } else if byte == b'}' {
            depth = depth
                .checked_sub(1)
                .ok_or_else(|| failure("tcl-lexical-balance-invalid"))?;
            if depth == 0 {
                return Ok(Some(cursor + 1));
            }
        }
        cursor += 1;
    }
    Ok(None)
}

fn balanced_brackets(
    source: &[u8],
    start: usize,
    end: usize,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let mut bracket_depth = 1_usize;
    let mut brace_depth = 0_usize;
    let mut quoted = [false; MAX_SCRIPT_DEPTH];
    let mut word_start = [true; MAX_SCRIPT_DEPTH];
    let mut command_start = [true; MAX_SCRIPT_DEPTH];
    let mut cursor = start + 1;
    while cursor < end {
        if cursor.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let Some(&byte) = source.get(cursor) else {
            return Ok(None);
        };
        if byte == b'\\' {
            if let Some(after) = line_continuation_end(source, cursor, end) {
                if brace_depth == 0 && !quoted[bracket_depth - 1] {
                    word_start[bracket_depth - 1] = true;
                }
                cursor = after;
            } else {
                if brace_depth == 0 && !quoted[bracket_depth - 1] {
                    word_start[bracket_depth - 1] = false;
                    command_start[bracket_depth - 1] = false;
                }
                cursor = (cursor + 2).min(end);
            }
            continue;
        }
        if brace_depth > 0 {
            match byte {
                b'{' => {
                    brace_depth = brace_depth
                        .checked_add(1)
                        .ok_or_else(|| failure("tcl-script-depth-limit"))?;
                }
                b'}' => brace_depth -= 1,
                _ => {}
            }
        } else if quoted[bracket_depth - 1] {
            match byte {
                b'"' => quoted[bracket_depth - 1] = false,
                b'[' => {
                    bracket_depth = bracket_depth
                        .checked_add(1)
                        .filter(|depth| *depth <= MAX_SCRIPT_DEPTH)
                        .ok_or_else(|| failure("tcl-script-depth-limit"))?;
                    quoted[bracket_depth - 1] = false;
                    word_start[bracket_depth - 1] = true;
                    command_start[bracket_depth - 1] = true;
                }
                _ => {}
            }
        } else {
            match byte {
                b' ' | b'\t' => word_start[bracket_depth - 1] = true,
                b'\r' | b'\n' | b';' => {
                    word_start[bracket_depth - 1] = true;
                    command_start[bracket_depth - 1] = true;
                }
                b'#' if command_start[bracket_depth - 1] => {
                    cursor = skip_comment(source, cursor, end);
                    continue;
                }
                b'{' if word_start[bracket_depth - 1] => {
                    brace_depth = 1;
                    word_start[bracket_depth - 1] = false;
                    command_start[bracket_depth - 1] = false;
                }
                b'"' if word_start[bracket_depth - 1] => {
                    quoted[bracket_depth - 1] = true;
                    word_start[bracket_depth - 1] = false;
                    command_start[bracket_depth - 1] = false;
                }
                b'[' => {
                    word_start[bracket_depth - 1] = false;
                    command_start[bracket_depth - 1] = false;
                    bracket_depth = bracket_depth
                        .checked_add(1)
                        .filter(|depth| *depth <= MAX_SCRIPT_DEPTH)
                        .ok_or_else(|| failure("tcl-script-depth-limit"))?;
                    quoted[bracket_depth - 1] = false;
                    word_start[bracket_depth - 1] = true;
                    command_start[bracket_depth - 1] = true;
                }
                b']' => {
                    bracket_depth = bracket_depth
                        .checked_sub(1)
                        .ok_or_else(|| failure("tcl-lexical-balance-invalid"))?;
                    if bracket_depth == 0 {
                        return Ok(Some(cursor + 1));
                    }
                }
                _ => {
                    word_start[bracket_depth - 1] = false;
                    command_start[bracket_depth - 1] = false;
                }
            }
        }
        cursor += 1;
    }
    Ok(None)
}

fn source_span(file: FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    let start = u64::try_from(start).map_err(|_| failure("tcl-source-offset-limit"))?;
    let end = u64::try_from(end).map_err(|_| failure("tcl-source-offset-limit"))?;
    SourceSpan::new(file, start, end).map_err(|_| failure("tcl-source-span-invalid"))
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed Tcl syntax label")
}

fn failure(value: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(value).expect("fixed Tcl diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, sync::Arc};

    use rootlight_adapter_sdk::{
        AnalysisLimits, AnalysisRequest, BatchThresholds, GenerationBoundSnapshot,
        MemoryAdmissionPolicy, ParseOutput, StreamLimits, execute_analysis, execute_parse,
    };
    use rootlight_adapter_treesitter::TreeSitterAnalyzer;
    use rootlight_ids::{GenerationId, derive_repository};
    use rootlight_ir::{
        BuildContextIdentity, ContainerRef, EntityKind, ExtensionSupport, IrLimits, OccurrenceRole,
        OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    };
    use rootlight_vfs::{RelativePath, RepositoryRoot};

    use super::*;

    #[test]
    fn procedures_namespaces_and_calls_keep_exact_source_spans() {
        let source = b"# proc hidden {} {}\nnamespace eval palette {\n  proc tint {x} {shade $x; return $x}\n  proc shade {x} {return $x}\n}\nproc render {} {palette::tint blue}\n";
        let parsed = scan_source(source, 32, &Cancellation::new()).expect("Tcl source parses");
        assert_eq!(parsed.items.len(), 4);
        assert_eq!(parsed.calls.len(), 4);
        assert_eq!(parsed.fact_count, 20);
        assert_eq!(parsed.items[2].parent_item, Some(0));
        assert_eq!(
            &source[parsed.items[2].name_start..parsed.items[2].name_end],
            b"tint"
        );
        let shade = parsed
            .calls
            .iter()
            .find(|call| &source[call.start..call.end] == b"shade")
            .expect("literal command call is retained");
        assert_eq!(shade.owner_item, Some(2));
    }

    #[test]
    fn data_words_do_not_create_procedures() {
        let source = b"set text {proc hidden {} {}}\nset quoted \"proc hidden {} {}\"\nset brackets [list \"]\" [list \"x\"]]\nset comment [list first; # ] is a comment\nlist second]\nset bare [list text{]\nproc visible {} {return ok}\n";
        let parsed = scan_source(source, 16, &Cancellation::new()).expect("Tcl source parses");
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(
            &source[parsed.items[0].name_start..parsed.items[0].name_end],
            b"visible"
        );
    }

    #[test]
    fn continued_data_words_do_not_become_commands() {
        for source in [
            &b"set palette [list \\\n  card 0 0 \\\n  border 1 2]\nproc draw {} {puts ok}\n"[..],
            &b"set palette [list \\\r\n  card 0 0 \\\r\n  border 1 2]\r\nproc draw {} {puts ok}\r\n"[..],
            &b"set palette [list \\\r  card 0 0 \\\r  border 1 2]\rproc draw {} {puts ok}\r"[..],
        ] {
            let parsed = scan_source(source, 32, &Cancellation::new()).expect("Tcl source parses");
            assert!(!parsed.lexical_incomplete);
            assert_eq!(parsed.items.len(), 1);
            let calls = parsed
                .calls
                .iter()
                .map(|call| &source[call.start..call.end])
                .collect::<Vec<_>>();
            assert_eq!(calls, [&b"set"[..], &b"list"[..], &b"puts"[..]]);
        }
    }

    #[test]
    fn continued_bare_words_stay_in_the_same_command() {
        for source in [
            &b"set value \\\n  data\n"[..],
            &b"set value \\\r\n  data\r\n"[..],
            &b"set value \\\r  data\r"[..],
        ] {
            let parsed = scan_source(source, 8, &Cancellation::new()).expect("Tcl source parses");
            assert_eq!(parsed.calls.len(), 1);
            assert_eq!(&source[parsed.calls[0].start..parsed.calls[0].end], b"set");
        }
    }

    #[test]
    fn continued_control_bodies_and_comments_keep_command_boundaries() {
        let source = b"# hidden \\\r\n  still hidden\r\nproc visible {} {\r\n  if {1} \\\r\n    {puts ok}\r\n  set value [list \\\r\n    data]\r\n}\r\n";
        let parsed = scan_source(source, 32, &Cancellation::new()).expect("Tcl source parses");
        assert_eq!(parsed.items.len(), 1);
        let mut calls = parsed
            .calls
            .iter()
            .map(|call| &source[call.start..call.end])
            .collect::<Vec<_>>();
        calls.sort_unstable();
        assert_eq!(calls, [&b"if"[..], &b"list"[..], &b"puts"[..], &b"set"[..]]);
    }

    #[test]
    fn command_substitutions_retain_executed_calls_without_braced_or_escaped_data() {
        let source = b"proc render {} {\n  set output [format \"%s\" [compute]]\n  puts \"result=[decorate [clock seconds]]\"\n  set escaped \\[ignored\\]\n  set literal {[ignored]}\n}\n";
        let parsed = scan_source(source, 16, &Cancellation::new()).expect("Tcl source parses");
        assert_eq!(parsed.items.len(), 1);
        let mut calls = parsed
            .calls
            .iter()
            .map(|call| {
                assert_eq!(call.owner_item, Some(0));
                std::str::from_utf8(&source[call.start..call.end]).expect("call is UTF-8")
            })
            .collect::<Vec<_>>();
        calls.sort_unstable();
        assert_eq!(
            calls,
            [
                "clock", "compute", "decorate", "format", "puts", "set", "set", "set"
            ]
        );
        assert_eq!(parsed.fact_count, 12);
    }

    #[test]
    fn control_command_scripts_retain_calls_without_treating_conditions_or_lists_as_scripts() {
        let source = b"proc dispatch {} {\n  if {condition} then {first} elseif {other} {second} else {third}\n  while {condition} {fourth}\n  for {initialize} {condition} {advance} {fifth}\n  foreach key {list data} value {more data} {sixth}\n  catch {seventh} result options\n  eval {eighth}\n  set literal {hidden}\n}\n";
        let parsed = scan_source(source, 64, &Cancellation::new()).expect("Tcl source parses");
        assert_eq!(parsed.items.len(), 1);
        let mut calls = parsed
            .calls
            .iter()
            .map(|call| {
                assert_eq!(call.owner_item, Some(0));
                std::str::from_utf8(&source[call.start..call.end]).expect("call is UTF-8")
            })
            .collect::<Vec<_>>();
        calls.sort_unstable();
        assert_eq!(
            calls,
            [
                "advance",
                "catch",
                "eighth",
                "eval",
                "fifth",
                "first",
                "for",
                "foreach",
                "fourth",
                "if",
                "initialize",
                "second",
                "set",
                "seventh",
                "sixth",
                "third",
                "while",
            ]
        );
    }

    #[test]
    fn duplicate_names_keep_candidate_call_sites() {
        let source = b"proc route/(edit) {} {shared}\nproc shared {} {}\nproc shared {} {}\nproc caller {} {route/(edit)}\n";
        let parsed = scan_source(source, 32, &Cancellation::new()).expect("Tcl source parses");
        assert_eq!(parsed.items.len(), 4);
        assert_eq!(parsed.calls.len(), 2);
        assert_eq!(
            &source[parsed.calls[0].start..parsed.calls[0].end],
            b"shared"
        );
        assert_eq!(
            &source[parsed.calls[1].start..parsed.calls[1].end],
            b"route/(edit)"
        );
    }

    #[test]
    fn dynamic_names_and_unbalanced_words_remain_incomplete() {
        let dynamic = b"proc $name {} {return 1}\nproc known {} {return 2}\n";
        let parsed = scan_source(dynamic, 16, &Cancellation::new()).expect("Tcl source parses");
        assert_eq!(parsed.items.len(), 1);
        let unbalanced = b"proc broken {} {return 1\nproc unseen {} {return 2}\n";
        let parsed = scan_source(unbalanced, 16, &Cancellation::new()).expect("Tcl source parses");
        assert!(parsed.lexical_incomplete);
        assert!(parsed.items.is_empty());
    }

    #[test]
    fn syntax_ceiling_rejects_partial_admission() {
        let source = b"proc one {} {}\nproc two {} {}\n";
        assert!(scan_source(source, 4, &Cancellation::new()).is_err());
        assert!(scan_source(b"set value [][][][]\n", 4, &Cancellation::new()).is_err());
        assert!(
            scan_source(
                b"while {1} {while {1} {while {1} {stop}}}",
                4,
                &Cancellation::new()
            )
            .is_err()
        );
        assert!(scan_source(b"\xff", 4, &Cancellation::new()).is_err());
    }
    #[test]
    fn formal_defaults_and_variadic_name_are_distinct_source_definitions() {
        let source = b"proc paint {w {color {dark blue}} args} {return $w}\n";
        let parsed = scan_source(source, 64, &Cancellation::new()).unwrap();
        assert!(parsed.formal_gaps.is_empty());
        assert_eq!(parsed.fact_count, 11);
        assert_eq!(parsed.parameters.len(), 3);
        let names: Vec<_> = parsed
            .parameters
            .iter()
            .map(|parameter| {
                assert_eq!(parameter.owner_item, 0);
                &source[parameter.name_start..parameter.name_end]
            })
            .collect();
        assert_eq!(names, [b"w".as_slice(), b"color", b"args"]);
        assert_eq!(
            &source[parsed.parameters[1].start..parsed.parameters[1].end],
            b"{color {dark blue}}"
        );
        assert!(
            parsed.parameters.iter().all(|parameter| parameter.name_end
                < source.iter().position(|byte| *byte == b'$').unwrap())
        );
    }

    #[test]
    fn formal_list_data_never_creates_default_expression_calls() {
        let source =
            br#"proc show {a {b "[never] $value; literal"} {c {{nested} list}}} {return $a}"#;
        let parsed = scan_source(source, 64, &Cancellation::new()).unwrap();
        assert_eq!(parsed.parameters.len(), 3);
        assert!(parsed.formal_gaps.is_empty());
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(
            &source[parsed.calls[0].start..parsed.calls[0].end],
            b"return"
        );
    }

    #[test]
    fn quoted_braced_names_and_static_command_words_keep_physical_bytes() {
        for source in [
            br#"proc quoted {"w" {{color} red} args} {}"#.as_slice(),
            br#"proc quoted "w {color red} args" {}"#.as_slice(),
        ] {
            let parsed = scan_source(source, 64, &Cancellation::new()).unwrap();
            let names: Vec<_> = parsed
                .parameters
                .iter()
                .map(|parameter| &source[parameter.name_start..parameter.name_end])
                .collect();
            assert_eq!(names, [b"w".as_slice(), b"color", b"args"]);
            assert!(parsed.formal_gaps.is_empty());
        }
        let parsed = scan_source(b"proc all args {}", 16, &Cancellation::new()).unwrap();
        assert_eq!(parsed.parameters.len(), 1);
        assert_eq!(
            &b"proc all args {}"[parsed.parameters[0].name_start..parsed.parameters[0].name_end],
            b"args"
        );
    }

    #[test]
    fn unicode_formals_keep_byte_offsets_and_each_written_procedure_owner() {
        let source = "namespace eval palette {\r\nproc λ {α {β {π λ}} args} {return $α}\r\nproc other {α} {}\r\n}";
        let parsed = scan_source(source.as_bytes(), 64, &Cancellation::new()).unwrap();
        let names: Vec<_> = parsed
            .parameters
            .iter()
            .map(|parameter| &source.as_bytes()[parameter.name_start..parameter.name_end])
            .collect();
        assert_eq!(
            names,
            ["α".as_bytes(), "β".as_bytes(), b"args", "α".as_bytes()]
        );
        assert_eq!(
            parsed
                .parameters
                .iter()
                .map(|parameter| parameter.owner_item)
                .collect::<Vec<_>>(),
            [1, 1, 1, 2]
        );
        assert_ne!(
            parsed.parameters[0].name_start,
            parsed.parameters[3].name_start
        );
        assert!(
            parsed
                .parameters
                .iter()
                .all(|parameter| source.is_char_boundary(parameter.name_start)
                    && source.is_char_boundary(parameter.name_end))
        );
    }

    #[test]
    fn malformed_formal_specs_do_not_preserve_a_valid_prefix() {
        for source in [
            br#"proc bad {valid {broken one two}} {}"#.as_slice(),
            br#"proc bad {valid {}} {}"#.as_slice(),
            br#"proc bad {valid "name"trailing} {}"#.as_slice(),
            br#"proc bad {valid {name "unterminated}} {}"#.as_slice(),
        ] {
            let parsed = scan_source(source, 64, &Cancellation::new()).unwrap();
            assert!(parsed.parameters.is_empty());
            assert_eq!(parsed.formal_gaps.len(), 1);
            assert_eq!(parsed.formal_gaps[0].code, "tcl-formal-list-not-analyzed");
            assert_eq!(
                &source[parsed.formal_gaps[0].start..parsed.formal_gaps[0].end],
                &source[9..source.len() - 3]
            );
        }
    }

    #[test]
    fn legal_unrepresentable_names_and_transforming_escapes_stay_explicit() {
        let source = br#"proc names {ok {a;b} {$literal} {[literal]} {{two words}}} {}"#;
        let parsed = scan_source(source, 64, &Cancellation::new()).unwrap();
        assert_eq!(parsed.parameters.len(), 1);
        assert_eq!(parsed.formal_gaps.len(), 4);
        assert!(
            parsed
                .formal_gaps
                .iter()
                .all(|gap| gap.code == "tcl-formal-name-not-analyzed")
        );
        for source in [
            br#"proc escaped {ok na\me} {}"#.as_slice(),
            b"proc continued {ok \\\n name} {}".as_slice(),
        ] {
            let parsed = scan_source(source, 64, &Cancellation::new()).unwrap();
            assert!(parsed.parameters.is_empty());
            assert_eq!(parsed.formal_gaps[0].code, "tcl-formal-escape-not-analyzed");
        }
    }

    #[test]
    fn formal_fact_ceiling_and_cancellation_are_not_silent_truncation() {
        let source = b"proc f {a b c} {}";
        assert_eq!(
            scan_source(source, 10, &Cancellation::new())
                .unwrap()
                .fact_count,
            10
        );
        assert!(scan_source(source, 9, &Cancellation::new()).is_err());
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(scan_source(source, 64, &cancelled).is_err());
    }

    struct NativeFixture {
        _directory: tempfile::TempDir,
        snapshot: rootlight_vfs::SourceSnapshot,
        source: SourceRef,
    }

    impl NativeFixture {
        fn new(bytes: &[u8]) -> Self {
            let directory = tempfile::tempdir().unwrap();
            fs::write(directory.path().join("parameters.tcl"), bytes).unwrap();
            let repository = derive_repository(b"native-tcl-formals").id();
            let root = RepositoryRoot::open(repository, directory.path()).unwrap();
            let snapshot = root
                .snapshot(
                    &RelativePath::parse(Path::new("parameters.tcl")).unwrap(),
                    4096,
                )
                .unwrap();
            let source = SourceRef::new(
                repository,
                GenerationId::from_bytes([9; 20]),
                SourceSpan::new(snapshot.file(), 0, u64::try_from(bytes.len()).unwrap()).unwrap(),
                snapshot.content_hash(),
                None,
            );
            Self {
                _directory: directory,
                snapshot,
                source,
            }
        }

        fn limits(nodes: usize, diagnostics: usize) -> AnalysisLimits {
            let batch = BatchThresholds::new(32, 32 * 1024, diagnostics, 4096).unwrap();
            let stream = StreamLimits::new(
                64,
                128,
                1024 * 1024,
                diagnostics,
                64 * 1024,
                64 * 1024,
                batch,
            )
            .unwrap();
            AnalysisLimits::new(
                4096,
                nodes,
                8,
                0,
                1024 * 1024,
                stream.clone(),
                stream,
                IrLimits::default(),
            )
            .unwrap()
        }

        fn parse(
            &self,
            nodes: usize,
            diagnostics: usize,
        ) -> (
            Result<usize, AdapterError>,
            Result<ParseOutput, AdapterError>,
        ) {
            let limits = Self::limits(nodes, diagnostics);
            let request = ParseRequest::new(
                GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
                LanguageId::new("tcl").unwrap(),
                EncodingId::utf8(),
                Vec::new(),
                &limits,
            )
            .unwrap();
            let provider = TclParseProvider::new(4096, nodes).unwrap();
            let cancel = Cancellation::new();
            (
                provider.required_syntax_fact_count(&request, &cancel),
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancel,
                ),
            )
        }
    }

    #[test]
    fn sdk_sink_preflight_usage_and_definition_parents_agree() {
        let fixture = NativeFixture::new(b"proc paint {w {color {dark blue}} args} {return $w}");
        let (required, output) = fixture.parse(11, 8);
        let output = output.unwrap();
        assert_eq!(required.unwrap(), 11);
        assert_eq!(output.facts().len(), 11);
        assert_eq!(output.report().resources().syntax_nodes(), 11);
        assert_eq!(output.report().resources().output_records(), 11);
        assert_eq!(output.report().resources().max_syntax_depth(), 3);
        assert_eq!(
            output.report().end().usage(),
            output.report().resources().stream()
        );
        for (domain, count) in [
            (FactDomain::Entities, 4),
            (FactDomain::Occurrences, 5),
            (FactDomain::Diagnostics, 1),
        ] {
            let actual = output
                .report()
                .coverage()
                .domains()
                .iter()
                .find(|row| row.domain() == domain)
                .unwrap();
            assert_eq!(
                (actual.discovered(), actual.indexed(), actual.skipped()),
                (count, count, 0)
            );
        }
        let procedure = output
            .facts()
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == "tcl.function.declaration")
            .unwrap();
        let body_use = b"proc paint {w {color {dark blue}} args} {return $w}"
            .iter()
            .position(|byte| *byte == b'$')
            .unwrap();
        for parameter in output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "tcl.parameter.declaration")
        {
            assert_eq!(parameter.parent(), Some(procedure.local_id()));
            let definitions: Vec<_> = output
                .facts()
                .iter()
                .filter(|fact| {
                    fact.parent() == Some(parameter.local_id())
                        && fact.syntax_kind().as_str() == "tcl.parameter.definition"
                })
                .collect();
            assert_eq!(definitions.len(), 1);
            assert!(definitions[0].span().end_byte() < u64::try_from(body_use).unwrap());
        }
        assert!(fixture.parse(10, 8).0.is_err());
        assert!(fixture.parse(10, 8).1.is_err());
    }

    #[test]
    fn formal_gap_diagnostics_are_exact_and_cannot_disappear_at_a_limit() {
        let bytes = b"proc names {ok {a;b} {$literal}} {}";
        let fixture = NativeFixture::new(bytes);
        let output = fixture.parse(64, 8).1.unwrap();
        let gaps: Vec<_> = output
            .diagnostics()
            .iter()
            .filter(|gap| gap.code().as_str() == "tcl-formal-name-not-analyzed")
            .collect();
        assert_eq!(gaps.len(), 2);
        for (gap, expected) in gaps.iter().zip([b"a;b".as_slice(), b"$literal"]) {
            let source = gap.source().unwrap();
            assert_eq!(source.repository(), fixture.source.repository());
            assert_eq!(source.generation(), fixture.source.generation());
            assert_eq!(source.content_hash(), fixture.source.content_hash());
            let range = source.span();
            assert_eq!(
                &bytes[usize::try_from(range.start_byte()).unwrap()
                    ..usize::try_from(range.end_byte()).unwrap()],
                expected
            );
        }
        assert!(fixture.parse(64, 1).1.is_err());
    }

    #[test]
    fn lowering_keeps_parameter_definition_and_exactly_one_procedure_contains_edge() {
        let bytes = b"proc paint {w {color red} args} {return $w}";
        let fixture = NativeFixture::new(bytes);
        let limits = NativeFixture::limits(64, 8);
        let parser = Arc::new(TclParseProvider::new(4096, 64).unwrap());
        let analyzer = TreeSitterAnalyzer::new(
            parser,
            ProducerIdentity::new(
                "native-tcl-formals",
                "1",
                content_hash(b"native-tcl-formals-config"),
            )
            .unwrap(),
            LanguageId::new("tcl").unwrap(),
            "native-tcl-formals",
            TclParseProvider::binary_digest(),
        )
        .unwrap();
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            LanguageId::new("tcl").unwrap(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"tcl-formals-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
        let document = output.document();
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "paint")
            .unwrap();
        for (name, expected_start, expected_end) in
            [("w", 12, 13), ("color", 15, 20), ("args", 26, 30)]
        {
            let parameter = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter && entity.canonical_name == name
                })
                .unwrap();
            assert_eq!(parameter.container, Some(ContainerRef::Entity(owner.id)));
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|edge| edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner.id)
                        && edge.object == RelationEndpoint::Entity(parameter.id))
                    .count(),
                1
            );
            let sites: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target
                            == (OccurrenceTarget::Resolved {
                                symbol: parameter.id,
                            })
                })
                .collect();
            assert_eq!(sites.len(), 1);
            let span = sites[0].source.span();
            assert_eq!(
                &bytes[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                name.as_bytes()
            );
            assert_eq!(
                (span.start_byte(), span.end_byte()),
                (expected_start, expected_end)
            );
        }
        let body_use = bytes.windows(2).position(|pair| pair == b"$w").unwrap() + 1;
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == u64::try_from(body_use).unwrap())
        );
    }

    #[test]
    fn resource_depth_matches_flat_calls_and_nested_parameter_ownership() {
        for (bytes, expected) in [
            (b"".as_slice(), 0),
            (b"return value".as_slice(), 1),
            (b"proc f {} {}".as_slice(), 2),
            (b"proc f {w} {}".as_slice(), 3),
            (
                b"namespace eval palette {proc outer {x} {proc inner {y} {return $y}}}".as_slice(),
                5,
            ),
        ] {
            let fixture = NativeFixture::new(bytes);
            let (required, output) = fixture.parse(64, 8);
            let output = output.unwrap();
            assert_eq!(required.unwrap(), output.facts().len());
            assert_eq!(output.report().resources().max_syntax_depth(), expected);
            assert_eq!(
                output.facts().iter().map(SyntaxFact::depth).max(),
                Some(expected)
            );
            for fact in output.facts() {
                if let Some(parent) = fact.parent() {
                    let owner = output
                        .facts()
                        .iter()
                        .find(|candidate| candidate.local_id() == parent)
                        .unwrap();
                    assert_eq!(fact.depth(), owner.depth() + 1);
                }
            }
        }
    }
}
