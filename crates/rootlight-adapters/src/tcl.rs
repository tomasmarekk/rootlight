//! First-party Tcl scanner for literal procedures, namespaces, and script bodies.
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

/// Bounded parser for source-written Tcl procedures and namespaces.
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
        }
        let usage = sink.staged_usage();
        let bytes = source.bytes().len();
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
                    parsed.items.len(),
                    parsed.items.len(),
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

struct ParsedSource {
    items: Vec<Item>,
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
    let mut lexical_incomplete = false;
    let mut script_index = 0;
    while let Some(script) = scripts.get(script_index).copied() {
        script_index += 1;
        let mut cursor = script.start;
        while cursor < script.end {
            if cursor.is_multiple_of(CHECK_INTERVAL) {
                cancellation.check()?;
            }
            while source
                .get(cursor)
                .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b';'))
                && cursor < script.end
            {
                cursor += 1;
            }
            if cursor >= script.end {
                break;
            }
            if source.get(cursor) == Some(&b'#') {
                while cursor < script.end && source.get(cursor) != Some(&b'\n') {
                    cursor += 1;
                }
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
                while cursor < script.end
                    && source
                        .get(cursor)
                        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r'))
                {
                    cursor += 1;
                }
                if cursor >= script.end
                    || source
                        .get(cursor)
                        .is_some_and(|byte| matches!(byte, b'\n' | b';'))
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
                && args.form == WordForm::Braced
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
        .and_then(|count| count.checked_add(command_calls.len()))
        .and_then(|count| count.checked_add(1))
        .filter(|count| *count <= max_syntax_nodes)
        .ok_or_else(|| failure("tcl-syntax-node-limit"))?;
    cancellation.check()?;
    Ok(ParsedSource {
        items,
        calls: command_calls,
        fact_count,
        lexical_incomplete,
    })
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
            Some(b'\\') => cursor = (cursor + 2).min(word.content_end),
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
        while cursor < end
            && source
                .get(cursor)
                .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r'))
        {
            cursor += 1;
        }
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
        if after < end && !source.get(after).is_some_and(|byte| word_separator(*byte)) {
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
                Some(b'\\') => cursor = (cursor + 2).min(end),
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
            cursor = (cursor + 2).min(end);
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
            if brace_depth == 0 && !quoted[bracket_depth - 1] {
                word_start[bracket_depth - 1] = false;
                command_start[bracket_depth - 1] = false;
            }
            cursor = (cursor + 2).min(end);
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
                b' ' | b'\t' | b'\r' => word_start[bracket_depth - 1] = true,
                b'\n' | b';' => {
                    word_start[bracket_depth - 1] = true;
                    command_start[bracket_depth - 1] = true;
                }
                b'#' if command_start[bracket_depth - 1] => {
                    while cursor < end && source.get(cursor) != Some(&b'\n') {
                        cursor += 1;
                    }
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
    use super::*;

    #[test]
    fn procedures_namespaces_and_calls_keep_exact_source_spans() {
        let source = b"# proc hidden {} {}\nnamespace eval palette {\n  proc tint {x} {shade $x; return $x}\n  proc shade {x} {return $x}\n}\nproc render {} {palette::tint blue}\n";
        let parsed = scan_source(source, 32, &Cancellation::new()).expect("Tcl source parses");
        assert_eq!(parsed.items.len(), 4);
        assert_eq!(parsed.calls.len(), 4);
        assert_eq!(parsed.fact_count, 16);
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
}
