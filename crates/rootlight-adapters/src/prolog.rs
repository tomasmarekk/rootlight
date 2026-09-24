//! First-party source parser for explicitly written Prolog clauses.
//! The scanner preserves byte spans and predicate arity without evaluating
//! directives or operators; unsupported heads remain covered only lexically.

use rootlight_adapter_sdk::{
    AdapterError, CoverageReport, DescriptorError, DiagnosticCode, DomainCoverage, EncodingId,
    LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider, ParseReport, ParseRequest,
    ResourceUsage, SinkError, StreamEnd, SyntaxFact, SyntaxFactBatch, SyntaxFactKind,
    SyntaxFactSink, SyntaxKindLabel, character_code_literal_len, predicate_head,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId, content_hash};
use rootlight_ir::{AnalysisTier, CoverageStatus, FactDomain, SourceSpan};

const CHECK_INTERVAL: usize = 4096;

/// Bounded parser for source-backed Prolog clause identities.
pub struct PrologParseProvider {
    capabilities: ParseCapabilities,
}

impl PrologParseProvider {
    /// Returns the content identity of this first-party frontend.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("prolog.rs"))
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
        Ok(syntax_fact_count(&parsed.groups))
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
            cancellation,
        )?;
        let fact_count = syntax_fact_count(&parsed.groups);
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
                head,
                2,
                label(definition_label),
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
        let source_bytes = source.bytes().len();
        let grouped_clauses = parsed.clause_count - parsed.groups.len();
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Bounded,
            source_bytes,
            source_bytes,
            grouped_clauses,
            vec![
                DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                DomainCoverage::new(
                    FactDomain::Entities,
                    CoverageStatus::Bounded,
                    parsed.clause_count,
                    parsed.groups.len(),
                    grouped_clauses,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClauseKind {
    Predicate,
    Nonterminal,
}

struct ParsedSource {
    groups: Vec<Clause>,
    clause_count: usize,
}

fn syntax_fact_count(clauses: &[Clause]) -> usize {
    1 + clauses.len() * 3
}

fn scan_source(
    source: &[u8],
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<ParsedSource, AdapterError> {
    if max_syntax_nodes == 0 {
        return Err(failure("prolog-syntax-node-limit"));
    }
    std::str::from_utf8(source).map_err(|_| failure("invalid-utf8"))?;
    let masked = mask_non_code(source, cancellation)?;
    let mut clauses = Vec::new();
    let mut clause_count = 0_usize;
    let mut previous_groupable = false;
    let mut sentence_start = 0_usize;
    let mut parens = 0_usize;
    let mut brackets = 0_usize;
    let mut braces = 0_usize;
    let mut valid = true;
    for (index, &byte) in masked.iter().enumerate() {
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
                && masked.get(index + 1).is_none_or(u8::is_ascii_whitespace) =>
            {
                if valid && let Some(clause) = clause_head(source, &masked, sentence_start, index) {
                    clause_count = clause_count
                        .checked_add(1)
                        .ok_or_else(|| failure("prolog-clause-count-limit"))?;
                    // A predicate identity needs one source-backed representative per
                    // contiguous clause group, while coverage records every omitted clause.
                    let same_group = previous_groupable
                        && clauses.last().is_some_and(|previous: &Clause| {
                            previous.arity == clause.arity
                                && previous.kind == clause.kind
                                && source[previous.head_start..previous.name_end]
                                    == source[clause.head_start..clause.name_end]
                        });
                    if !same_group {
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
                }
                sentence_start = index + 1;
                valid = true;
            }
            _ => {}
        }
    }
    Ok(ParsedSource {
        groups: clauses,
        clause_count,
    })
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
    })
}

fn mask_non_code(source: &[u8], cancellation: &Cancellation) -> Result<Vec<u8>, AdapterError> {
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
        let quoted_atom_start = !line_comment
            && block_depth == 0
            && quote.is_none()
            && byte == b'\''
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
            2
        } else if let Some(length) = character_code {
            length
        } else if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
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
        if quoted_atom_start {
            // Keep only the opening marker so a quoted functor can start a
            // sentence; its punctuation and delimiters stay masked as data.
            *masked
                .get_mut(index)
                .ok_or_else(|| failure("prolog-source-span-invalid"))? = b'\'';
        }
        index = end;
    }
    Ok(masked)
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
    use super::*;

    #[test]
    fn clause_scanner_groups_contiguous_predicates_without_losing_coverage_counts() {
        let source = b"% comment\n:- module(graph, [edge/2]).\nedge(a, b).\nedge(A, B) :-\n  route(A, B).\nlink([a,b], 'x.y').\n";
        let parsed = scan_source(source, 20, &Cancellation::new()).expect("valid clauses");
        let heads = parsed
            .groups
            .iter()
            .map(|clause| &source[clause.head_start..clause.head_end])
            .collect::<Vec<_>>();
        assert_eq!(heads, [b"edge(a, b)".as_slice(), b"link([a,b], 'x.y')"]);
        assert_eq!(parsed.clause_count, 3);
        assert_eq!(syntax_fact_count(&parsed.groups), 7);
        assert!(scan_source(source, 6, &Cancellation::new()).is_err());
    }

    #[test]
    fn intervening_sentences_and_arity_split_predicate_groups() {
        let source = b"edge(a).\nedge(b).\n:- dynamic edge/1.\nedge(c).\nedge(a, b).\n";
        let parsed = scan_source(source, 20, &Cancellation::new()).expect("valid clauses");
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
        let parsed = scan_source(source, 10, &Cancellation::new()).expect("bounded source");
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
        let parsed = scan_source(source, 10, &Cancellation::new()).expect("bounded source");
        assert_eq!(parsed.clause_count, 3);
        assert_eq!(parsed.groups.len(), 2);
        assert_eq!(parsed.groups[0].kind, ClauseKind::Nonterminal);
        assert_eq!(parsed.groups[1].kind, ClauseKind::Predicate);
        assert_eq!(syntax_fact_count(&parsed.groups), 7);
    }

    #[test]
    fn quoted_functor_head_keeps_source_span_without_comment_ghosts() {
        let source = b"% '<urn:ghost>'(A).\n'<urn:edge>'(A, B) :- edge(A, B).\n";
        let parsed = scan_source(source, 4, &Cancellation::new()).expect("quoted source parses");
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
        let parsed = scan_source(source, 10, &Cancellation::new()).expect("character codes scan");
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
        let parsed = scan_source(source.as_bytes(), 4, &Cancellation::new()).expect("UTF-8 source");
        assert_eq!(parsed.groups.len(), 1);
        let clause = parsed.groups[0];
        assert_eq!(
            &source.as_bytes()[clause.head_start..clause.head_end],
            "λ(α, β)".as_bytes()
        );
        assert!(scan_source(b"edge(a).\n\xff", 4, &Cancellation::new()).is_err());
    }
}
