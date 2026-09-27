//! First-party structural analysis of literal INI section and key declarations.
//! Source spans identify authored headers and assignments; dialect-specific value
//! expansion and ambiguous continuation syntax remain explicit bounded gaps.

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
const MAX_GAP_ACCOUNTED_BYTES: usize = 256;

/// Bounded native provider for source-backed INI sections and keys.
pub struct IniParseProvider {
    capabilities: ParseCapabilities,
}

impl IniParseProvider {
    /// Returns the content identity of this first-party frontend.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("ini.rs"))
    }

    /// Creates the parser under the caller's source and syntax-node ceilings.
    ///
    /// # Errors
    /// Returns [`DescriptorError`] when a ceiling is zero.
    ///
    /// # Panics
    /// Panics only if the fixed `ini` language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        let language = LanguageId::new("ini").expect("fixed INI language label");
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

    /// Counts the syntax facts emitted by parsing before stream admission.
    ///
    /// # Errors
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
            request.limits().ir().max_string_bytes,
            cancellation,
        )?
        .fact_count)
    }
}

impl ParseProvider for IniParseProvider {
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
            label("ini.source_file"),
        ));
        let mut next_id = 2_u64;
        let mut section_id = None;
        for declaration in &parsed.declarations {
            cancellation.check()?;
            let id = next_id;
            next_id = next_id
                .checked_add(2)
                .ok_or_else(|| failure("ini-syntax-node-limit"))?;
            let (parent, depth, kind) = match declaration.kind {
                DeclarationKind::Section => {
                    section_id = Some(id);
                    (1, 1, "ini.section.declaration")
                }
                DeclarationKind::Key => (
                    section_id.unwrap_or(1),
                    if section_id.is_some() { 2 } else { 1 },
                    "ini.key.declaration",
                ),
            };
            facts.push(SyntaxFact::new(
                id,
                Some(parent),
                SyntaxFactKind::Declaration,
                source_span(file, declaration.start, declaration.end)?,
                depth,
                label(kind),
            ));
            facts.push(SyntaxFact::new(
                id + 1,
                Some(id),
                SyntaxFactKind::Occurrence,
                source_span(file, declaration.name_start, declaration.name_end)?,
                depth + 1,
                label(match declaration.kind {
                    DeclarationKind::Section => "ini.section.definition",
                    DeclarationKind::Key => "ini.key.definition",
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
        let diagnostic_budget = gap_diagnostic_budget(request, sink.staged_usage());
        if parsed.skipped_regions > 0 && diagnostic_budget > 0 {
            let diagnostic_count = parsed.skipped_regions.min(diagnostic_budget);
            let mut diagnostics = Vec::new();
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
                    .ok_or_else(|| failure("ini-gap-accounting"))?
                    .start;
                diagnostics.push(gap_diagnostic(
                    source.source_ref(),
                    start,
                    parsed.last_gap_end,
                    "ini-gap-diagnostic-limit",
                )?);
            }
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), Vec::new(), diagnostics),
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
                DomainCoverage::new(FactDomain::Relations, CoverageStatus::Bounded, 0, 0, 0)?,
            ],
        )?;
        Ok(ParseReport::new(
            coverage,
            ResourceUsage::new(source_bytes, facts.len(), facts.len(), 3, None, usage),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeclarationKind {
    Section,
    Key,
}

struct Declaration {
    kind: DeclarationKind,
    start: usize,
    end: usize,
    name_start: usize,
    name_end: usize,
}

struct ParsedSource {
    declarations: Vec<Declaration>,
    fact_count: usize,
    skipped_regions: usize,
    gaps: Vec<Gap>,
    last_gap_end: usize,
}

#[derive(Clone, Copy)]
enum GapKind {
    Section,
    Key,
    NameLimit,
    Statement,
    Value,
    Continuation,
    Indentation,
}

impl GapKind {
    const fn code(self) -> &'static str {
        match self {
            Self::Section => "ini-section-header-parse-error",
            Self::Key => "ini-key-name-parse-error",
            Self::NameLimit => "ini-name-length-limit",
            Self::Statement => "ini-statement-not-analyzed",
            Self::Value => "ini-value-semantics-not-analyzed",
            Self::Continuation => "ini-continuation-not-analyzed",
            Self::Indentation => "ini-indented-key-dialect-unverified",
        }
    }
}

struct Gap {
    start: usize,
    end: usize,
    kind: GapKind,
}

#[derive(Clone, Copy)]
struct ScanLimits {
    max_syntax_nodes: usize,
    max_name_bytes: usize,
}

fn record_gap(parsed: &mut ParsedSource, start: usize, end: usize, kind: GapKind) {
    parsed.skipped_regions = parsed.skipped_regions.saturating_add(1);
    parsed.last_gap_end = end;
    if parsed.gaps.len() < MAX_SCOPED_GAPS {
        parsed.gaps.push(Gap { start, end, kind });
    }
}

fn push_declaration(
    parsed: &mut ParsedSource,
    declaration: Declaration,
    max_syntax_nodes: usize,
) -> Result<usize, AdapterError> {
    let count = parsed
        .fact_count
        .checked_add(2)
        .filter(|count| *count <= max_syntax_nodes)
        .ok_or_else(|| failure("ini-syntax-node-limit"))?;
    parsed
        .declarations
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    let index = parsed.declarations.len();
    parsed.declarations.push(declaration);
    parsed.fact_count = count;
    Ok(index)
}

fn scan_source(
    source: &[u8],
    max_syntax_nodes: usize,
    max_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<ParsedSource, AdapterError> {
    if max_syntax_nodes == 0 {
        return Err(failure("ini-syntax-node-limit"));
    }
    std::str::from_utf8(source).map_err(|_| failure("invalid-utf8"))?;
    let mut parsed = ParsedSource {
        declarations: Vec::new(),
        fact_count: 1,
        skipped_regions: 0,
        gaps: Vec::new(),
        last_gap_end: 0,
    };
    let mut current_section = None;
    let mut unknown_section = false;
    let mut continued_value = false;
    let mut line_start = 0;
    let limits = ScanLimits {
        max_syntax_nodes,
        max_name_bytes,
    };
    for (index, &byte) in source.iter().enumerate() {
        if index % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        if byte == b'\n' {
            scan_line(
                source.get(line_start..index).unwrap_or_default(),
                line_start,
                &mut current_section,
                &mut unknown_section,
                &mut continued_value,
                &mut parsed,
                limits,
            )?;
            line_start = index + 1;
        }
    }
    if line_start < source.len() {
        scan_line(
            source.get(line_start..).unwrap_or_default(),
            line_start,
            &mut current_section,
            &mut unknown_section,
            &mut continued_value,
            &mut parsed,
            limits,
        )?;
    }
    if let Some(section) = current_section {
        parsed.declarations[section].end = source.len();
    }
    cancellation.check()?;
    Ok(parsed)
}

fn scan_line(
    raw_line: &[u8],
    offset: usize,
    current_section: &mut Option<usize>,
    unknown_section: &mut bool,
    continued_value: &mut bool,
    parsed: &mut ParsedSource,
    limits: ScanLimits,
) -> Result<(), AdapterError> {
    let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
    let end = offset
        .checked_add(line.len())
        .ok_or_else(|| failure("ini-source-offset-limit"))?;
    let leading = line.len() - line.trim_ascii_start().len();
    let trimmed = line.trim_ascii_start();
    if *continued_value {
        if !trimmed.is_empty() {
            record_gap(parsed, offset, end, GapKind::Continuation);
        }
        if leading != 0 {
            *continued_value = trimmed.trim_ascii_end().ends_with(b"\\");
            return Ok(());
        }
        *continued_value = false;
    }
    if trimmed.is_empty() || matches!(trimmed.first(), Some(b';' | b'#')) {
        return Ok(());
    }
    if trimmed.starts_with(b"[") {
        if let Some(section) = current_section.take() {
            parsed.declarations[section].end = offset;
        }
        let close = trimmed.iter().position(|byte| *byte == b']');
        if let Some(close) = close {
            let name = trimmed.get(1..close).unwrap_or_default();
            let name_leading = name.len() - name.trim_ascii_start().len();
            let name = name.trim_ascii();
            let suffix = trimmed
                .get(close + 1..)
                .unwrap_or_default()
                .trim_ascii_start();
            if static_name(name)
                && (suffix.is_empty() || matches!(suffix.first(), Some(b';' | b'#')))
            {
                if name.len() > limits.max_name_bytes {
                    *unknown_section = true;
                    record_gap(parsed, offset + leading, end, GapKind::NameLimit);
                    return Ok(());
                }
                let name_start = offset + leading + 1 + name_leading;
                let section = push_declaration(
                    parsed,
                    Declaration {
                        kind: DeclarationKind::Section,
                        start: offset + leading,
                        end,
                        name_start,
                        name_end: name_start + name.len(),
                    },
                    limits.max_syntax_nodes,
                )?;
                *current_section = Some(section);
                *unknown_section = false;
                return Ok(());
            }
        }
        *unknown_section = true;
        record_gap(parsed, offset + leading, end, GapKind::Section);
        return Ok(());
    }
    if *unknown_section {
        record_gap(parsed, offset + leading, end, GapKind::Statement);
        return Ok(());
    }
    let Some(equal) = trimmed.iter().position(|byte| *byte == b'=') else {
        record_gap(parsed, offset + leading, end, GapKind::Statement);
        return Ok(());
    };
    let name = trimmed.get(..equal).unwrap_or_default().trim_ascii_end();
    if !static_name(name) {
        record_gap(parsed, offset + leading, end, GapKind::Key);
        return Ok(());
    }
    if name.len() > limits.max_name_bytes {
        record_gap(parsed, offset + leading, end, GapKind::NameLimit);
        return Ok(());
    }
    if leading != 0 {
        // An indentation-based multiline dialect could interpret this line as
        // a value continuation. Preserve the assignment shape but not certainty.
        record_gap(parsed, offset, end, GapKind::Indentation);
    }
    push_declaration(
        parsed,
        Declaration {
            kind: DeclarationKind::Key,
            start: offset + leading,
            end,
            name_start: offset + leading,
            name_end: offset + leading + name.len(),
        },
        limits.max_syntax_nodes,
    )?;
    let value = trimmed.get(equal + 1..).unwrap_or_default();
    if !value.trim_ascii().is_empty() {
        record_gap(parsed, offset + leading + equal + 1, end, GapKind::Value);
        *continued_value = value.trim_ascii_end().ends_with(b"\\");
    }
    Ok(())
}

fn static_name(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|name| {
        !name.is_empty()
            && name.chars().all(|character| {
                !character.is_control()
                    && !matches!(
                        character,
                        '[' | ']' | '=' | ';' | '#' | '%' | '$' | '{' | '}'
                    )
            })
    })
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
        DiagnosticCode::new(code).map_err(|_| failure("ini-diagnostic-code"))?,
        DiagnosticSeverity::Warning,
        Some(source),
        CoverageStatus::Bounded,
    ))
}

fn source_span(file: FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    let start = u64::try_from(start).map_err(|_| failure("ini-source-offset-limit"))?;
    let end = u64::try_from(end).map_err(|_| failure("ini-source-offset-limit"))?;
    SourceSpan::new(file, start, end).map_err(|_| failure("ini-source-span-invalid"))
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed INI syntax label")
}

fn failure(value: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(value).expect("fixed INI diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_sections_and_keys_keep_exact_source_spans() {
        let source = b"; header\r\n[Network]\r\nHost = server\r\nPort=443\r\n[UI] # comment\r\nTheme=dark\r\n";
        let parsed = scan_source(source, 16, 64, &Cancellation::new()).unwrap();
        let names: Vec<_> = parsed
            .declarations
            .iter()
            .map(|declaration| &source[declaration.name_start..declaration.name_end])
            .collect();
        assert_eq!(
            names,
            [b"Network".as_slice(), b"Host", b"Port", b"UI", b"Theme",]
        );
        assert_eq!(parsed.fact_count, 11);
        assert_eq!(parsed.skipped_regions, 3);
        assert!(parsed.declarations[0].end <= parsed.declarations[3].start);
        assert!(scan_source(source, 10, 64, &Cancellation::new()).is_err());
    }

    #[test]
    fn malformed_headers_quarantine_keys_until_next_verified_section() {
        let source = b"[valid]\na=1\n[broken\nphantom=2\n[again]\nb=3\n";
        let parsed = scan_source(source, 16, 64, &Cancellation::new()).unwrap();
        let names: Vec<_> = parsed
            .declarations
            .iter()
            .map(|declaration| &source[declaration.name_start..declaration.name_end])
            .collect();
        assert_eq!(names, [b"valid".as_slice(), b"a", b"again", b"b"]);
        assert!(parsed.gaps.iter().any(|gap| {
            gap.kind.code() == "ini-section-header-parse-error"
                && &source[gap.start..gap.end] == b"[broken"
        }));
        assert!(parsed.gaps.iter().any(|gap| {
            gap.kind.code() == "ini-statement-not-analyzed"
                && &source[gap.start..gap.end] == b"phantom=2"
        }));
    }

    #[test]
    fn comments_values_and_continuations_never_invent_keys() {
        let source = b"# fake=1\n[section]\n  actual=1\nkey=before\\\n  phantom=2\n  continuation\nother=3\n";
        let parsed = scan_source(source, 16, 64, &Cancellation::new()).unwrap();
        let names: Vec<_> = parsed
            .declarations
            .iter()
            .map(|declaration| &source[declaration.name_start..declaration.name_end])
            .collect();
        assert_eq!(names, [b"section".as_slice(), b"actual", b"key", b"other"]);
        assert!(parsed.skipped_regions >= 6);
        assert!(parsed.gaps.iter().any(|gap| {
            gap.kind.code() == "ini-indented-key-dialect-unverified"
                && &source[gap.start..gap.end] == b"  actual=1"
        }));
        assert!(parsed.gaps.iter().any(|gap| {
            gap.kind.code() == "ini-continuation-not-analyzed"
                && &source[gap.start..gap.end] == b"  phantom=2"
        }));
    }

    #[test]
    fn trailing_path_separator_does_not_hide_an_unindented_section() {
        let source = b"[first]\nRoot=C:\\folder\\\n[second]\nName=value\n";
        let parsed = scan_source(source, 16, 64, &Cancellation::new()).unwrap();
        let names: Vec<_> = parsed
            .declarations
            .iter()
            .map(|declaration| &source[declaration.name_start..declaration.name_end])
            .collect();
        assert_eq!(names, [b"first".as_slice(), b"Root", b"second", b"Name"]);
        assert!(parsed.gaps.iter().any(|gap| {
            gap.kind.code() == "ini-continuation-not-analyzed"
                && &source[gap.start..gap.end] == b"[second]"
        }));
    }

    #[test]
    fn many_unanalyzed_values_keep_bounded_gap_summary() {
        let source = b"key=value\n".repeat(MAX_SCOPED_GAPS + 8);
        let parsed = scan_source(&source, 128, 64, &Cancellation::new()).unwrap();
        assert_eq!(parsed.skipped_regions, MAX_SCOPED_GAPS + 8);
        assert_eq!(parsed.gaps.len(), MAX_SCOPED_GAPS);
        assert_eq!(parsed.last_gap_end, source.len() - 1);
    }

    #[test]
    fn oversized_names_have_explicit_gaps_instead_of_disappearing() {
        let source = b"[LongSection]\na=1\n[ok]\nLongKey=2\nshort=3\n";
        let parsed = scan_source(source, 16, 5, &Cancellation::new()).unwrap();
        let names: Vec<_> = parsed
            .declarations
            .iter()
            .map(|declaration| &source[declaration.name_start..declaration.name_end])
            .collect();
        assert_eq!(names, [b"ok".as_slice(), b"short"]);
        let limited: Vec<_> = parsed
            .gaps
            .iter()
            .filter(|gap| gap.kind.code() == "ini-name-length-limit")
            .map(|gap| &source[gap.start..gap.end])
            .collect();
        assert_eq!(limited, [b"[LongSection]".as_slice(), b"LongKey=2"]);
        assert!(static_name("key_π".as_bytes()));
        assert!(!static_name("bad\u{0085}".as_bytes()));
    }

    #[test]
    fn diagnostic_codes_fit_stream_budget() {
        for kind in [
            GapKind::Section,
            GapKind::Key,
            GapKind::NameLimit,
            GapKind::Statement,
            GapKind::Value,
            GapKind::Continuation,
            GapKind::Indentation,
        ] {
            assert!(kind.code().len() <= MAX_GAP_CODE_BYTES);
        }
        assert!("ini-gap-diagnostic-limit".len() <= MAX_GAP_CODE_BYTES);
    }
}
