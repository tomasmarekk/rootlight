//! First-party source parser for explicitly written Prolog clauses.
//! The scanner preserves byte spans and predicate arity without evaluating
//! directives or operators. Unrecognized sentences and unanalyzed rule bodies
//! retain scoped diagnostics; preserved declaration groups are not skipped work.
//! Closed own-file direct goals use separate required source witnesses.

mod calls;

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, CoverageReport, DescriptorError, DiagnosticCode,
    DomainCoverage, EncodingId, LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider,
    ParseReport, ParseRequest, ResourceUsage, SinkError, StreamEnd, SyntaxFact, SyntaxFactBatch,
    SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel, character_code_literal_len, predicate_head,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId, content_hash};
use rootlight_ir::{
    AnalysisTier, CoverageStatus, DiagnosticSeverity, FactDomain, SourceRef, SourceSpan,
};

const CHECK_INTERVAL: usize = 4096;

/// Bounded parser for source-backed Prolog clause identities.
pub struct PrologParseProvider {
    capabilities: ParseCapabilities,
}

impl PrologParseProvider {
    /// Returns the content identity of this first-party frontend.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        {
            let mut input = [0_u8; 64];
            input[..32].copy_from_slice(content_hash(include_bytes!("prolog.rs")).as_bytes());
            input[32..].copy_from_slice(content_hash(include_bytes!("prolog/calls.rs")).as_bytes());
            content_hash(&input)
        }
    }

    /// Creates a parser with the caller's source and syntax-node ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError`] when a ceiling is zero.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed `prolog` language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        let language = LanguageId::new("prolog").expect("fixed Prolog language label");
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

    /// Counts the same syntax facts that parsing will emit for preflight admission.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for invalid UTF-8, cancellation, or a syntax or
    /// diagnostic ceiling.
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
            request.limits().syntax_stream().max_diagnostics(),
            cancellation,
        )?;
        Ok(syntax_fact_count(&parsed.groups) + parsed.calls.len() * 3)
    }
}

impl ParseProvider for PrologParseProvider {
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
            request.limits().syntax_stream().max_diagnostics(),
            cancellation,
        )?;
        let fact_count = syntax_fact_count(&parsed.groups) + parsed.calls.len() * 3;
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
            label("prolog.source_file"),
        ));
        for (index, clause) in parsed.groups.iter().enumerate() {
            cancellation.check()?;
            let (declaration_label, signature_label, definition_label) = match clause.kind {
                ClauseKind::Predicate => (
                    "prolog.predicate.declaration",
                    "prolog.predicate.signature",
                    "prolog.predicate.definition",
                ),
                ClauseKind::Nonterminal => (
                    "prolog.nonterminal.declaration",
                    "prolog.nonterminal.signature",
                    "prolog.nonterminal.definition",
                ),
            };
            let local_id = u64::try_from(index)
                .ok()
                .and_then(|index| index.checked_mul(3))
                .and_then(|offset| offset.checked_add(2))
                .ok_or_else(|| failure("prolog-syntax-node-limit"))?;
            let head = source_span(file, clause.head_start, clause.head_end)?;
            let written_head =
                std::str::from_utf8(&source.bytes()[clause.head_start..clause.head_end])
                    .map_err(|_| failure("invalid-utf8"))?;
            let definition_end = clause
                .head_start
                .checked_add(written_head.trim_end().len())
                .ok_or_else(|| failure("prolog-source-offset-limit"))?;
            facts.push(SyntaxFact::new(
                local_id,
                Some(1),
                SyntaxFactKind::Declaration,
                source_span(file, clause.head_start, clause.clause_end)?,
                1,
                label(declaration_label),
            ));
            facts.push(SyntaxFact::new(
                local_id + 1,
                Some(local_id),
                SyntaxFactKind::Signature,
                head,
                2,
                label(signature_label),
            ));
            facts.push(SyntaxFact::new(
                local_id + 2,
                Some(local_id),
                SyntaxFactKind::Occurrence,
                source_span(file, clause.head_start, definition_end)?,
                2,
                label(definition_label),
            ));
        }
        for (index, call) in parsed.calls.iter().enumerate() {
            cancellation.check()?;
            let id = u64::try_from(syntax_fact_count(&parsed.groups))
                .ok()
                .and_then(|base| {
                    u64::try_from(index)
                        .ok()
                        .and_then(|index| index.checked_mul(3))
                        .and_then(|offset| base.checked_add(offset))
                })
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| failure("prolog-syntax-node-limit"))?;
            let owner = u64::try_from(call.owner)
                .ok()
                .and_then(|value| value.checked_mul(3))
                .and_then(|value| value.checked_add(2))
                .ok_or_else(|| failure("prolog-syntax-node-limit"))?;
            facts.push(SyntaxFact::new(
                id,
                Some(owner),
                SyntaxFactKind::Occurrence,
                source_span(file, call.start, call.name_end)?,
                2,
                label("prolog.direct_goal.reference"),
            ));
            facts.push(SyntaxFact::new(
                id + 1,
                Some(owner),
                SyntaxFactKind::Signature,
                source_span(file, call.start, call.end)?,
                2,
                label("prolog.goal_context.signature"),
            ));
            facts.push(SyntaxFact::new(
                id + 2,
                Some(1),
                SyntaxFactKind::Signature,
                source_span(file, parsed.groups[call.target].head_start, call.name_end)?,
                1,
                label("prolog.goal_target.signature"),
            ));
        }
        let fact_batch_size = request
            .limits()
            .syntax_stream()
            .batch()
            .max_records()
            .min(32);
        for batch in facts.chunks(fact_batch_size) {
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
        let diagnostic_batch_size = request
            .limits()
            .syntax_stream()
            .batch()
            .max_diagnostics()
            .min(32);
        for batch in parsed.gaps.chunks(diagnostic_batch_size) {
            cancellation.check()?;
            let mut diagnostics = Vec::new();
            diagnostics
                .try_reserve_exact(batch.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            for gap in batch {
                let full_source = source.source_ref();
                diagnostics.push(AdapterDiagnostic::new(
                    DiagnosticCode::new(gap.code).expect("fixed Prolog diagnostic code"),
                    DiagnosticSeverity::Warning,
                    Some(SourceRef::new(
                        full_source.repository(),
                        full_source.generation(),
                        source_span(file, gap.start, gap.end)?,
                        full_source.content_hash(),
                        None,
                    )),
                    CoverageStatus::Bounded,
                ));
            }
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), Vec::new(), diagnostics),
                cancellation,
            )?;
        }
        let usage = sink.staged_usage();
        let source_bytes = source.bytes().len();
        debug_assert!(parsed.clause_count >= parsed.groups.len());
        // Entity work counts preserved declaration groups, not their clauses.
        // Rejected sentences cannot establish an entity count; their uncertainty
        // and unexamined bodies remain separately source-scoped diagnostics.
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Bounded,
            source_bytes,
            source_bytes,
            parsed.gaps.len(),
            vec![
                DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                DomainCoverage::new(
                    FactDomain::Entities,
                    CoverageStatus::Bounded,
                    parsed.groups.len(),
                    parsed.groups.len(),
                    0,
                )?,
                DomainCoverage::new(
                    FactDomain::Diagnostics,
                    CoverageStatus::Bounded,
                    parsed.gaps.len(),
                    parsed.gaps.len(),
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
struct Clause {
    head_start: usize,
    head_end: usize,
    clause_end: usize,
    name_end: usize,
    arity: usize,
    kind: ClauseKind,
    has_body: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClauseKind {
    Predicate,
    Nonterminal,
}

struct ParsedSource {
    groups: Vec<Clause>,
    clause_count: usize,
    calls: Vec<calls::DirectCall>,
    gaps: Vec<SourceGap>,
}

struct SourceGap {
    start: usize,
    end: usize,
    code: &'static str,
}

struct MaskedSource {
    bytes: Vec<u8>,
    incomplete_start: Option<usize>,
}

fn syntax_fact_count(clauses: &[Clause]) -> usize {
    1 + clauses.len() * 3
}

fn scan_source(
    source: &[u8],
    max_syntax_nodes: usize,
    max_diagnostics: usize,
    cancellation: &Cancellation,
) -> Result<ParsedSource, AdapterError> {
    if max_syntax_nodes == 0 {
        return Err(failure("prolog-syntax-node-limit"));
    }
    std::str::from_utf8(source).map_err(|_| failure("invalid-utf8"))?;
    let masked = mask_non_code(source, cancellation)?;
    let mut clauses = Vec::new();
    let mut gaps = Vec::new();
    let mut clause_count = 0_usize;
    let mut previous_groupable = false;
    let mut sentence_start = 0_usize;
    let mut parens = 0_usize;
    let mut brackets = 0_usize;
    let mut braces = 0_usize;
    let mut valid = true;
    for (index, &byte) in masked.bytes.iter().enumerate() {
        if index % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        match byte {
            b'(' => parens = parens.saturating_add(1),
            b'[' => brackets = brackets.saturating_add(1),
            b'{' => braces = braces.saturating_add(1),
            b')' => valid &= decrement(&mut parens),
            b']' => valid &= decrement(&mut brackets),
            b'}' => valid &= decrement(&mut braces),
            b'.' if parens == 0
                && brackets == 0
                && braces == 0
                && masked
                    .bytes
                    .get(index + 1)
                    .is_none_or(u8::is_ascii_whitespace) =>
            {
                if valid
                    && let Some(clause) = clause_head(source, &masked.bytes, sentence_start, index)
                {
                    clause_count = clause_count
                        .checked_add(1)
                        .ok_or_else(|| failure("prolog-clause-count-limit"))?;
                    // One identity spans all contiguous clauses; retaining only
                    // the first clause would hide alternative definitions from source.read.
                    let same_group = previous_groupable
                        && clauses.last().is_some_and(|previous: &Clause| {
                            previous.arity == clause.arity
                                && previous.kind == clause.kind
                                && source[previous.head_start..previous.name_end]
                                    == source[clause.head_start..clause.name_end]
                        });
                    if same_group {
                        let previous = clauses
                            .last_mut()
                            .expect("a matching clause group already exists");
                        previous.clause_end = index + 1;
                        previous.has_body |= clause.has_body;
                    } else {
                        let next_count = clauses
                            .len()
                            .checked_add(1)
                            .and_then(|count| count.checked_mul(3))
                            .and_then(|count| count.checked_add(1))
                            .filter(|count| *count <= max_syntax_nodes)
                            .ok_or_else(|| failure("prolog-syntax-node-limit"))?;
                        debug_assert!(next_count > 1);
                        clauses
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        clauses.push(Clause {
                            clause_end: index + 1,
                            ..clause
                        });
                    }
                    previous_groupable = true;
                } else {
                    previous_groupable = false;
                    let start = content_start(&masked.bytes, sentence_start, index, cancellation)?
                        .unwrap_or(index);
                    push_gap(
                        &mut gaps,
                        SourceGap {
                            start,
                            end: index + 1,
                            code: if valid && start != index {
                                "prolog-sentence-not-analyzed"
                            } else {
                                "prolog-unbalanced-sentence"
                            },
                        },
                        max_diagnostics,
                    )?;
                }
                sentence_start = index + 1;
                valid = true;
            }
            _ => {}
        }
    }
    let incomplete_start =
        content_start(&masked.bytes, sentence_start, source.len(), cancellation)?.or(masked
            .incomplete_start
            .filter(|start| *start >= sentence_start));
    if let Some(start) = incomplete_start {
        push_gap(
            &mut gaps,
            SourceGap {
                start,
                end: source.len(),
                code: "prolog-incomplete-sentence",
            },
            max_diagnostics,
        )?;
    }
    append_body_gaps(
        &clauses,
        &mut gaps,
        source.len(),
        max_diagnostics,
        cancellation,
    )?;
    let calls = calls::certify(source, &masked, &clauses, max_syntax_nodes, cancellation)?;
    Ok(ParsedSource {
        calls,
        groups: clauses,
        clause_count,
        gaps,
    })
}

fn append_body_gaps(
    groups: &[Clause],
    gaps: &mut Vec<SourceGap>,
    source_len: usize,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut body_groups = 0_usize;
    for group in groups {
        cancellation.check()?;
        if group.has_body {
            body_groups = body_groups
                .checked_add(1)
                .ok_or_else(|| failure("prolog-clause-count-limit"))?;
        }
    }
    if body_groups == 0 {
        return Ok(());
    }
    if body_groups > maximum.saturating_sub(gaps.len()) {
        // Repetitive body qualifications must not consume more diagnostics than
        // the independent stream permits. This is explicitly file-wide, not a
        // truncated list of allegedly covered group bodies.
        return push_gap(
            gaps,
            SourceGap {
                start: 0,
                end: source_len,
                code: "prolog-file-body-semantics-partial",
            },
            maximum,
        );
    }
    for group in groups {
        cancellation.check()?;
        if group.has_body {
            push_gap(
                gaps,
                SourceGap {
                    start: group.head_start,
                    end: group.clause_end,
                    code: "prolog-body-semantics-partial",
                },
                maximum,
            )?;
        }
    }
    Ok(())
}

fn content_start(
    masked: &[u8],
    start: usize,
    end: usize,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    for index in start..end {
        if index.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if masked
            .get(index)
            .is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn push_gap(gaps: &mut Vec<SourceGap>, gap: SourceGap, maximum: usize) -> Result<(), AdapterError> {
    if gaps.len() >= maximum {
        return Err(failure("prolog-diagnostic-limit"));
    }
    gaps.try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    gaps.push(gap);
    Ok(())
}

fn decrement(depth: &mut usize) -> bool {
    if let Some(next) = depth.checked_sub(1) {
        *depth = next;
        true
    } else {
        false
    }
}

fn clause_head(source: &[u8], masked: &[u8], start: usize, end: usize) -> Option<Clause> {
    let start = (start..end).find(|&index| {
        masked
            .get(index)
            .is_some_and(|byte| !byte.is_ascii_whitespace())
    })?;
    let body = masked.get(start..end)?;
    if body.starts_with(b":-") || body.starts_with(b"?-") {
        return None;
    }
    let mut parens = 0_usize;
    let mut brackets = 0_usize;
    let mut braces = 0_usize;
    let mut head_end = end;
    let mut kind = ClauseKind::Predicate;
    let mut index = start;
    while index < end {
        let tail = masked.get(index..end)?;
        if parens == 0 && brackets == 0 && braces == 0 {
            if tail.starts_with(b":+") {
                return None;
            }
            if tail.starts_with(b"-->") {
                kind = ClauseKind::Nonterminal;
                head_end = index;
                break;
            }
            if tail.starts_with(b":-") {
                head_end = index;
                break;
            }
        }
        match *tail.first()? {
            b'(' => parens = parens.checked_add(1)?,
            b')' => parens = parens.checked_sub(1)?,
            b'[' => brackets = brackets.checked_add(1)?,
            b']' => brackets = brackets.checked_sub(1)?,
            b'{' => braces = braces.checked_add(1)?,
            b'}' => braces = braces.checked_sub(1)?,
            _ => {}
        }
        index += 1;
    }
    let head = std::str::from_utf8(source.get(start..head_end)?).ok()?;
    let (name, arity) = predicate_head(head)?;
    Some(Clause {
        head_start: start,
        head_end,
        clause_end: end + 1,
        name_end: start + name.len(),
        arity,
        kind,
        has_body: head_end != end,
    })
}

fn mask_non_code(source: &[u8], cancellation: &Cancellation) -> Result<MaskedSource, AdapterError> {
    let mut masked = Vec::new();
    masked
        .try_reserve_exact(source.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    masked.extend_from_slice(source);
    let mut index = if source.starts_with(b"\xef\xbb\xbf") {
        masked
            .get_mut(..3)
            .ok_or_else(|| failure("prolog-source-span-invalid"))?
            .fill(b' ');
        3
    } else {
        0
    };
    let shebang_start = index;
    let mut block_depth = 0_usize;
    let mut line_comment = false;
    let mut quote = None;
    let mut incomplete_start = None;
    while index < source.len() {
        if index % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        let tail = source
            .get(index..)
            .ok_or_else(|| failure("prolog-source-span-invalid"))?;
        let byte = *tail
            .first()
            .ok_or_else(|| failure("prolog-source-span-invalid"))?;
        let character_code = if !line_comment && block_depth == 0 && quote.is_none() {
            character_code_literal_len(source, index)
        } else {
            None
        };
        let quoted_literal_start = !line_comment
            && block_depth == 0
            && quote.is_none()
            && matches!(byte, b'\'' | b'"' | b'`')
            && character_code.is_none();
        let length = if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            1
        } else if block_depth > 0 {
            if tail.starts_with(b"/*") {
                block_depth = block_depth
                    .checked_add(1)
                    .ok_or_else(|| failure("prolog-comment-depth-limit"))?;
                2
            } else if tail.starts_with(b"*/") {
                block_depth -= 1;
                if block_depth == 0 {
                    incomplete_start = None;
                }
                2
            } else {
                1
            }
        } else if let Some(delimiter) = quote {
            if byte == b'\\' {
                2.min(tail.len())
            } else if byte == delimiter {
                if tail.get(1) == Some(&delimiter) {
                    2
                } else {
                    quote = None;
                    incomplete_start = None;
                    1
                }
            } else {
                1
            }
        } else if byte == b'%' || index == shebang_start && tail.starts_with(b"#!") {
            line_comment = true;
            if byte == b'%' { 1 } else { 2 }
        } else if tail.starts_with(b"/*") {
            block_depth = 1;
            incomplete_start = Some(index);
            2
        } else if let Some(length) = character_code {
            length
        } else if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
            incomplete_start = Some(index);
            1
        } else {
            index += 1;
            continue;
        };
        let end = index
            .checked_add(length)
            .ok_or_else(|| failure("prolog-source-offset-limit"))?;
        for byte in masked
            .get_mut(index..end)
            .ok_or_else(|| failure("prolog-source-span-invalid"))?
        {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
        if quoted_literal_start {
            // Preserve each literal's start so closed-source proofs cannot
            // erase unsupported syntax as layout; interiors remain opaque.
            *masked
                .get_mut(index)
                .ok_or_else(|| failure("prolog-source-span-invalid"))? = byte;
        }
        index = end;
    }
    Ok(MaskedSource {
        bytes: masked,
        incomplete_start,
    })
}

fn source_span(file: FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    let start = u64::try_from(start).map_err(|_| failure("prolog-source-offset-limit"))?;
    let end = u64::try_from(end).map_err(|_| failure("prolog-source-offset-limit"))?;
    SourceSpan::new(file, start, end).map_err(|_| failure("prolog-source-span-invalid"))
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed Prolog syntax label")
}

fn failure(value: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(value).expect("fixed Prolog diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        time::{Duration, Instant},
    };

    use rootlight_adapter_sdk::{
        AnalysisLimits, BatchThresholds, GenerationBoundSnapshot, MemoryAdmissionPolicy,
        ParseOutput, StreamLimits, execute_parse,
    };
    use rootlight_ir::{IrLimits, SourceRef};
    use rootlight_vfs::{RelativePath, RepositoryRoot};

    use super::*;

    #[test]
    fn preserved_clause_groups_are_not_skipped_parser_work() {
        let source = b"edge(a).\nedge(b).\nedge(c).\n";
        let (required, output) = parse_fixture(source, 4, 64);
        let output = output.expect("written clause groups parse through the real sink");
        assert_eq!(required.expect("preflight accepts the same groups"), 4);
        assert_eq!(output.facts().len(), 4);
        assert_eq!(output.report().resources().syntax_nodes(), 4);
        assert_eq!(output.report().resources().output_records(), 4);
        assert_eq!(output.report().coverage().skipped_regions(), 0);
        let entities = output
            .report()
            .coverage()
            .domains()
            .iter()
            .find(|domain| domain.domain() == FactDomain::Entities)
            .expect("entity work is declared");
        assert_eq!(entities.status(), CoverageStatus::Bounded);
        assert_eq!(entities.discovered(), 1);
        assert_eq!(entities.indexed(), 1);
        assert_eq!(entities.skipped(), 0);
        assert!(output.diagnostics().is_empty());
        let parsed = scan_source(source, 4, 64, &Cancellation::new()).expect("clauses are scanned");
        assert_eq!(parsed.clause_count, 3);
        assert_eq!(parsed.groups.len(), 1);
        let declaration = output
            .facts()
            .iter()
            .find(|fact| fact.kind() == SyntaxFactKind::Declaration)
            .expect("the entire written group is retained");
        assert_eq!(declaration.span().start_byte(), 0);
        assert_eq!(
            usize::try_from(declaration.span().end_byte()).unwrap(),
            source.len() - 1
        );
    }

    #[test]
    fn rule_and_dcg_body_uncertainty_survives_clause_grouping() {
        let source =
            b"edge(a).\nedge(A) :- link(A).\nedge(b).\nphrase(X) --> [X].\nphrase(Y) --> [Y].\n";
        let (required, output) = parse_fixture(source, 7, 64);
        let output = output.expect("group identities coexist with explicit body uncertainty");
        assert_eq!(required.expect("preflight retains two groups"), 7);
        assert_eq!(output.facts().len(), 7);
        assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
        assert_eq!(output.report().coverage().skipped_regions(), 2);
        assert_eq!(output.diagnostics().len(), 2);
        let parsed = scan_source(source, 7, 64, &Cancellation::new()).expect("raw clauses scan");
        assert_eq!(parsed.clause_count, 5);
        assert_eq!(parsed.groups.len(), 2);
        assert!(parsed.groups.iter().all(|group| group.has_body));
        let entities = output
            .report()
            .coverage()
            .domains()
            .iter()
            .find(|domain| domain.domain() == FactDomain::Entities)
            .unwrap();
        assert_eq!(
            (
                entities.discovered(),
                entities.indexed(),
                entities.skipped()
            ),
            (2, 2, 0)
        );
        for (diagnostic, group) in output.diagnostics().iter().zip(&parsed.groups) {
            assert_eq!(diagnostic.code().as_str(), "prolog-body-semantics-partial");
            let span = diagnostic
                .source()
                .expect("body uncertainty has preserved group extent")
                .span();
            assert_eq!(
                usize::try_from(span.start_byte()).unwrap(),
                group.head_start
            );
            assert_eq!(usize::try_from(span.end_byte()).unwrap(), group.clause_end);
        }
        assert!(
            output
                .report()
                .coverage()
                .domains()
                .iter()
                .all(|domain| !matches!(
                    domain.domain(),
                    FactDomain::Occurrences | FactDomain::Relations
                ))
        );
    }

    #[test]
    fn unsupported_sentences_keep_exact_scoped_coverage_evidence() {
        let source = "\u{feff}% lead\r\nedge(a).\r\n:- dynamic edge/1.\r\nedge(b).\r\nmodule:edge(c).\r\nleft ++ right.\r\nquery(X) :+ answer(X).\r\n?- edge(a).\r\n";
        let expected_gaps = [
            ":- dynamic edge/1.",
            "module:edge(c).",
            "left ++ right.",
            "query(X) :+ answer(X).",
            "?- edge(a).",
        ];
        let (required, output) = parse_fixture(source.as_bytes(), 7, 64);
        let output = output.expect("unsupported sentences retain bounded output");
        assert_eq!(required.expect("preflight preserves accepted groups"), 7);
        assert_eq!(output.facts().len(), 7);
        assert_eq!(output.report().resources().stream().records(), 7);
        assert_eq!(output.report().resources().stream().diagnostics(), 5);
        assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
        assert_eq!(output.report().coverage().skipped_regions(), 5);
        assert_eq!(output.diagnostics().len(), 5);
        let domains = output.report().coverage().domains();
        let entities = domains
            .iter()
            .find(|domain| domain.domain() == FactDomain::Entities)
            .expect("accepted group work is declared");
        assert_eq!(
            (
                entities.discovered(),
                entities.indexed(),
                entities.skipped()
            ),
            (2, 2, 0)
        );
        let diagnostics = domains
            .iter()
            .find(|domain| domain.domain() == FactDomain::Diagnostics)
            .expect("unrecognized sentence work is declared");
        assert_eq!(
            (
                diagnostics.discovered(),
                diagnostics.indexed(),
                diagnostics.skipped()
            ),
            (5, 5, 0)
        );
        for (diagnostic, expected) in output.diagnostics().iter().zip(expected_gaps) {
            assert_eq!(diagnostic.code().as_str(), "prolog-sentence-not-analyzed");
            assert_eq!(diagnostic.severity(), DiagnosticSeverity::Warning);
            assert_eq!(diagnostic.coverage_effect(), CoverageStatus::Bounded);
            let evidence = diagnostic.source().expect("gap has exact source evidence");
            assert_eq!(evidence.content_hash(), content_hash(source.as_bytes()));
            assert_eq!(evidence.span().file(), output.facts()[0].span().file());
            assert_eq!(
                evidence.repository(),
                "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap()
            );
            assert_eq!(
                evidence.generation(),
                "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
                    .parse()
                    .unwrap()
            );
            let start = usize::try_from(evidence.span().start_byte()).unwrap();
            let end = usize::try_from(evidence.span().end_byte()).unwrap();
            assert_eq!(&source.as_bytes()[start..end], expected.as_bytes());
        }
    }

    #[test]
    fn malformed_and_incomplete_source_stays_scoped_without_invented_groups() {
        for (tail, expected_code) in [
            ("bad(X)).", "prolog-unbalanced-sentence"),
            (".", "prolog-unbalanced-sentence"),
            ("unfinished(X", "prolog-incomplete-sentence"),
            ("unfinished(X.", "prolog-incomplete-sentence"),
            ("/* outer /* inner */", "prolog-incomplete-sentence"),
            ("edge('unterminated).", "prolog-incomplete-sentence"),
            ("edge(A) :- 'unterminated.", "prolog-incomplete-sentence"),
            ("edge(A) :- /* unterminated.", "prolog-incomplete-sentence"),
        ] {
            let source = format!("ok(a).\n{tail}");
            let (required, output) = parse_fixture(source.as_bytes(), 4, 64);
            let output = output.expect("partial source retains only the proven group");
            assert_eq!(required.expect("preflight agrees"), 4, "{tail}");
            assert_eq!(output.facts().len(), 4, "{tail}");
            assert_eq!(output.report().coverage().skipped_regions(), 1, "{tail}");
            assert_eq!(output.diagnostics().len(), 1, "{tail}");
            let diagnostic = &output.diagnostics()[0];
            assert_eq!(diagnostic.code().as_str(), expected_code, "{tail}");
            let evidence = diagnostic
                .source()
                .expect("malformed region is source-scoped");
            assert_eq!(evidence.span().start_byte(), 7, "{tail}");
            assert_eq!(
                usize::try_from(evidence.span().end_byte()).unwrap(),
                source.len(),
                "{tail}"
            );
            assert_eq!(
                evidence.content_hash(),
                content_hash(source.as_bytes()),
                "{tail}"
            );
        }
    }

    #[test]
    fn comments_only_source_does_not_become_skipped_entity_work() {
        let source =
            "\u{feff}#!/usr/bin/env prolog\r\n% edge(fake).\r\n/* outer /* inner. */ */\r\n";
        let (required, output) = parse_fixture(source.as_bytes(), 1, 64);
        let output = output.expect("only the immutable source root is emitted");
        assert_eq!(required.expect("preflight sees no declarations"), 1);
        assert_eq!(output.facts().len(), 1);
        assert!(output.diagnostics().is_empty());
        assert_eq!(output.report().coverage().skipped_regions(), 0);
        assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
    }

    #[test]
    fn preflight_and_parser_preserve_syntax_and_diagnostic_ceilings() {
        let (required, output) = parse_fixture(b"edge(a).\nlink(b).\n", 6, 64);
        for error in [required.unwrap_err(), output.unwrap_err()] {
            assert!(matches!(error, AdapterError::ProviderFailed { code }
                if code.as_str() == "prolog-syntax-node-limit"));
        }
        let (required, output) = parse_fixture(b":- dynamic edge/1.\n?- edge(a).\n", 1, 1);
        for error in [required.unwrap_err(), output.unwrap_err()] {
            assert!(matches!(error, AdapterError::ProviderFailed { code }
                if code.as_str() == "prolog-diagnostic-limit"));
        }
        let source = ":- dynamic edge/1.\n".repeat(33);
        let (required, output) = parse_fixture(source.as_bytes(), 1, 64);
        let output = output.expect("diagnostic-only batches commit within existing ceilings");
        assert_eq!(required.expect("diagnostics do not invent syntax facts"), 1);
        assert_eq!(output.facts().len(), 1);
        assert_eq!(output.diagnostics().len(), 33);
        assert_eq!(output.report().coverage().skipped_regions(), 33);
        assert_eq!(output.report().resources().stream().batches(), 3);
        assert_eq!(output.report().resources().stream().records(), 1);
        assert_eq!(output.report().resources().stream().diagnostics(), 33);
        assert_eq!(
            output.report().end().usage(),
            output.report().resources().stream()
        );
    }

    #[test]
    fn many_rule_groups_keep_facts_with_explicit_file_wide_body_qualification() {
        let source = (0..41)
            .map(|index| format!("rule_{index}(X) :- next(X).\n"))
            .collect::<String>();
        for (prefix, maximum_diagnostics) in [("", 1), (":- dynamic next/1.\n", 2)] {
            let source = format!("{prefix}{source}");
            let (required, output) = parse_fixture(source.as_bytes(), 124, maximum_diagnostics);
            let output = output.expect("repetitive body uncertainty preserves all admitted groups");
            assert_eq!(
                required.expect("preflight preserves all written groups"),
                124
            );
            assert_eq!(output.facts().len(), 124);
            assert_eq!(output.report().resources().output_records(), 124);
            assert_eq!(output.report().resources().syntax_nodes(), 124);
            assert_eq!(output.diagnostics().len(), maximum_diagnostics);
            assert_eq!(
                output.report().coverage().skipped_regions(),
                maximum_diagnostics
            );
            assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
            let parsed = scan_source(
                source.as_bytes(),
                124,
                maximum_diagnostics,
                &Cancellation::new(),
            )
            .expect("raw clause accounting is retained");
            assert_eq!(parsed.clause_count, 41);
            assert_eq!(parsed.groups.len(), 41);
            assert!(parsed.groups.iter().all(|group| group.has_body));
            let diagnostic = output
                .diagnostics()
                .iter()
                .find(|diagnostic| {
                    diagnostic.code().as_str() == "prolog-file-body-semantics-partial"
                })
                .expect("aggregation explicitly qualifies the whole file");
            let evidence = diagnostic
                .source()
                .expect("qualification has exact immutable evidence");
            assert_eq!(evidence.span().start_byte(), 0);
            assert_eq!(
                usize::try_from(evidence.span().end_byte()).unwrap(),
                source.len()
            );
            assert_eq!(evidence.content_hash(), content_hash(source.as_bytes()));
            let domain = output
                .report()
                .coverage()
                .domains()
                .iter()
                .find(|domain| domain.domain() == FactDomain::Diagnostics)
                .unwrap();
            assert_eq!(
                (domain.discovered(), domain.indexed(), domain.skipped()),
                (maximum_diagnostics, maximum_diagnostics, 0)
            );
            if !prefix.is_empty() {
                let unsupported = output
                    .diagnostics()
                    .iter()
                    .find(|diagnostic| diagnostic.code().as_str() == "prolog-sentence-not-analyzed")
                    .expect("specific unsupported evidence is not suppressed by aggregation");
                assert_eq!(unsupported.source().unwrap().span().start_byte(), 0);
                assert_eq!(
                    usize::try_from(unsupported.source().unwrap().span().end_byte()).unwrap(),
                    prefix.len() - 1
                );
            }
        }
        let source = b"first(X) :- next(X).\nsecond(X) --> [X].\n";
        let (_, output) = parse_fixture(source, 7, 2);
        let output = output.expect("exactly fitting group qualifications remain scoped");
        assert_eq!(output.diagnostics().len(), 2);
        assert!(
            output
                .diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.code().as_str() == "prolog-body-semantics-partial")
        );
    }

    #[test]
    fn clause_scanner_groups_contiguous_predicates_without_losing_coverage_counts() {
        let source = b"% comment\n:- module(graph, [edge/2]).\nedge(a, b).\nedge(A, B) :-\n  route(A, B).\nlink([a,b], 'x.y').\n";
        let parsed = scan_source(source, 20, 64, &Cancellation::new()).expect("valid clauses");
        let heads = parsed
            .groups
            .iter()
            .map(|clause| &source[clause.head_start..clause.head_end])
            .collect::<Vec<_>>();
        assert_eq!(heads, [b"edge(a, b)".as_slice(), b"link([a,b], 'x.y')"]);
        assert_eq!(parsed.clause_count, 3);
        assert_eq!(syntax_fact_count(&parsed.groups), 7);
        assert_eq!(
            &source[parsed.groups[0].head_start..parsed.groups[0].clause_end],
            b"edge(a, b).\nedge(A, B) :-\n  route(A, B)."
        );
        assert!(scan_source(source, 6, 64, &Cancellation::new()).is_err());
    }

    #[test]
    fn intervening_sentences_and_arity_split_predicate_groups() {
        let source = b"edge(a).\nedge(b).\n:- dynamic edge/1.\nedge(c).\nedge(a, b).\n";
        let parsed = scan_source(source, 20, 64, &Cancellation::new()).expect("valid clauses");
        assert_eq!(parsed.clause_count, 4);
        assert_eq!(parsed.groups.len(), 3);
        assert_eq!(
            parsed
                .groups
                .iter()
                .map(|group| group.arity)
                .collect::<Vec<_>>(),
            [1, 1, 2]
        );
    }

    #[test]
    fn comments_literals_and_unclosed_source_cannot_invent_predicates() {
        let source = b"/* nested /* edge(fake). */ */\nrule(X) --> [X].\nquery(X) :+ answer(X).\nactual('ignored ).', [a,b]).\n% ghost(X).\nunfinished(X\n";
        let parsed = scan_source(source, 10, 64, &Cancellation::new()).expect("bounded source");
        assert_eq!(parsed.groups.len(), 2);
        assert_eq!(parsed.groups[0].kind, ClauseKind::Nonterminal);
        let clause = parsed.groups[1];
        assert_eq!(
            &source[clause.head_start..clause.head_end],
            b"actual('ignored ).', [a,b])"
        );
    }

    #[test]
    fn dcg_and_predicate_groups_never_merge_at_the_same_written_arity() {
        let source = b"phrase(X) --> [X].\nphrase(Y) --> [Y].\nphrase(X) :- X = plain.\n";
        let parsed = scan_source(source, 10, 64, &Cancellation::new()).expect("bounded source");
        assert_eq!(parsed.clause_count, 3);
        assert_eq!(parsed.groups.len(), 2);
        assert_eq!(parsed.groups[0].kind, ClauseKind::Nonterminal);
        assert_eq!(parsed.groups[1].kind, ClauseKind::Predicate);
        assert_eq!(syntax_fact_count(&parsed.groups), 7);
    }

    #[test]
    fn quoted_functor_head_keeps_source_span_without_comment_ghosts() {
        let source = b"% '<urn:ghost>'(A).\n'<urn:edge>'(A, B) :- edge(A, B).\n";
        let parsed =
            scan_source(source, 4, 64, &Cancellation::new()).expect("quoted source parses");
        assert_eq!(parsed.clause_count, 1);
        assert_eq!(parsed.groups.len(), 1);
        let clause = parsed.groups[0];
        assert_eq!(
            &source[clause.head_start..clause.head_end],
            b"'<urn:edge>'(A, B) "
        );
        assert_eq!(&source[clause.head_start..clause.name_end], b"'<urn:edge>'");
    }

    #[test]
    fn character_code_literals_do_not_hide_following_clauses() {
        let source = b"token(0'@, In, Out) :- Out = In.\nnext(0'(, 0',).\n'<urn:edge>'(A, B).\n";
        let parsed =
            scan_source(source, 10, 64, &Cancellation::new()).expect("character codes scan");
        assert_eq!(parsed.clause_count, 3);
        assert_eq!(parsed.groups.len(), 3);
        assert_eq!(
            &source[parsed.groups[2].head_start..parsed.groups[2].name_end],
            b"'<urn:edge>'"
        );
    }

    #[test]
    fn byte_spans_survive_unicode_bom_and_crlf() {
        let source = "\u{feff}% note\r\nλ(α, β).\r\n";
        let parsed =
            scan_source(source.as_bytes(), 4, 64, &Cancellation::new()).expect("UTF-8 source");
        assert_eq!(parsed.groups.len(), 1);
        let clause = parsed.groups[0];
        assert_eq!(
            &source.as_bytes()[clause.head_start..clause.head_end],
            "λ(α, β)".as_bytes()
        );
        assert!(scan_source(b"edge(a).\n\xff", 4, 64, &Cancellation::new()).is_err());
    }

    fn parse_fixture(
        bytes: &[u8],
        maximum_nodes: usize,
        maximum_diagnostics: usize,
    ) -> (
        Result<usize, AdapterError>,
        Result<ParseOutput, AdapterError>,
    ) {
        parse_fixture_with_batch(bytes, maximum_nodes, maximum_diagnostics, 32)
    }

    fn parse_fixture_with_batch(
        bytes: &[u8],
        maximum_nodes: usize,
        maximum_diagnostics: usize,
        batch_records: usize,
    ) -> (
        Result<usize, AdapterError>,
        Result<ParseOutput, AdapterError>,
    ) {
        let temporary = tempfile::tempdir().expect("fixture directory is created");
        fs::write(temporary.path().join("source.pl"), bytes).expect("fixture source is written");
        let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
            .parse()
            .expect("fixture repository identity parses");
        let root = RepositoryRoot::open(repository_id, temporary.path())
            .expect("fixture repository is opened");
        let path =
            RelativePath::parse(Path::new("source.pl")).expect("fixture relative path is valid");
        let snapshot = root
            .snapshot(&path, 4096)
            .expect("fixture snapshot is stable");
        let source = SourceRef::new(
            repository_id,
            "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
                .parse()
                .expect("fixture generation identity parses"),
            SourceSpan::new(
                snapshot.file(),
                0,
                u64::try_from(bytes.len()).expect("small source"),
            )
            .expect("fixture full source span is valid"),
            snapshot.content_hash(),
            None,
        );
        let batch = BatchThresholds::new(batch_records, 32 * 1024, maximum_diagnostics, 4096)
            .expect("fixture batch ceilings are valid");
        let stream = StreamLimits::new(
            64,
            128,
            1024 * 1024,
            maximum_diagnostics,
            64 * 1024,
            64 * 1024,
            batch,
        )
        .expect("fixture stream ceilings are valid");
        let limits = AnalysisLimits::new(
            4096,
            maximum_nodes,
            8,
            0,
            1024 * 1024,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .expect("fixture analysis limits are valid");
        let request = ParseRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source).expect("fixture source binds"),
            LanguageId::new("prolog").expect("fixed language label"),
            EncodingId::utf8(),
            Vec::new(),
            &limits,
        )
        .expect("fixture parse request is valid");
        let provider =
            PrologParseProvider::new(4096, maximum_nodes).expect("provider ceilings are valid");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("fixture deadline derives"),
        );
        let required = provider.required_syntax_fact_count(&request, &cancellation);
        let output = execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        );
        (required, output)
    }

    #[test]
    fn original_sixteen_facts_honor_eight_record_batches() {
        let source = include_bytes!("../tests/fixtures/prolog_head/age.pl");
        assert_eq!(source.len(), 353);
        assert_eq!(&source[200..208], b"birthDay");
        let (required, parsed) = parse_fixture_with_batch(source, 16, 64, 8);
        assert_eq!(
            required.expect("original required proof cardinality is sixteen"),
            16
        );
        let parsed = parsed.expect("the SDK accepts all sixteen facts in batches of at most eight");
        assert_eq!(parsed.facts().len(), 16);
        assert_eq!(parsed.report().resources().syntax_nodes(), 16);
        assert_eq!(parsed.report().end().usage().records(), 16);
        assert_eq!(parsed.report().end().next_sequence(), 3);
        assert_eq!(parsed.diagnostics().len(), 2);
        let goals: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "prolog.direct_goal.reference")
            .collect();
        assert_eq!(goals.len(), 2);
        assert_eq!(
            (goals[0].span().start_byte(), goals[0].span().end_byte()),
            (200, 208)
        );
        assert!(parsed.diagnostics().iter().all(|diagnostic| {
            diagnostic
                .source()
                .is_some_and(|evidence| evidence.content_hash() == content_hash(source))
        }));
    }
}
