//! First-party Elm top-level structural parser.
//!
//! The lexer masks comments and literals without changing byte offsets. It
//! emits only identities proven by a top-level header, leaving expression and
//! binding coverage explicitly bounded.

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

/// Bounded native parser for source-backed Elm declaration identities.
pub struct ElmParseProvider {
    capabilities: ParseCapabilities,
}

impl ElmParseProvider {
    /// Returns the content identity of this first-party frontend implementation.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("elm.rs"))
    }

    /// Creates a provider using the caller's source and syntax-node ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError`] for zero limits.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed `elm` language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        let language = LanguageId::new("elm").expect("fixed Elm language label");
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

    /// Counts syntax identities required before partition admission.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] for invalid UTF-8, cancellation, or a syntax
    /// ceiling reached by the same scan used for emission.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        let headers = scan_headers(request.source().bytes(), cancellation)?;
        let count = syntax_fact_count(&headers);
        if count > request.limits().max_syntax_nodes()
            || count > self.capabilities.max_syntax_nodes()
        {
            return Err(failure("elm-syntax-node-limit"));
        }
        Ok(required_syntax_fact_count(&headers))
    }
}

impl ParseProvider for ElmParseProvider {
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
        let headers = scan_headers(source.bytes(), cancellation)?;
        let fact_count = syntax_fact_count(&headers);
        if fact_count > request.limits().max_syntax_nodes()
            || fact_count > self.capabilities.max_syntax_nodes()
        {
            return Err(failure("elm-syntax-node-limit"));
        }
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
            label("elm.source_file"),
        ));
        for (index, header) in headers.iter().enumerate() {
            cancellation.check()?;
            let local_id = u64::try_from(index)
                .ok()
                .and_then(|index| index.checked_mul(2))
                .and_then(|index| index.checked_add(2))
                .ok_or_else(|| failure("elm-syntax-node-limit"))?;
            let end = headers
                .get(index + 1)
                .map_or(source.bytes().len(), |next| next.line_start);
            let span = source_span(file, header.line_start, end)?;
            facts.push(SyntaxFact::new(
                local_id,
                Some(1),
                header.kind,
                span,
                1,
                label(header.label),
            ));
            facts.push(SyntaxFact::new(
                local_id + 1,
                Some(local_id),
                SyntaxFactKind::Occurrence,
                source_span(file, header.name_start, header.name_end)?,
                2,
                label(if header.kind == SyntaxFactKind::Import {
                    "elm.name.reference"
                } else {
                    "elm.name.definition"
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
        let source_bytes = source.bytes().len();
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Bounded,
            source_bytes,
            source_bytes,
            0,
            vec![
                DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                DomainCoverage::new(
                    FactDomain::Entities,
                    CoverageStatus::Bounded,
                    headers.len(),
                    headers.len(),
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

#[derive(Clone, Copy)]
struct Header {
    line_start: usize,
    name_start: usize,
    name_end: usize,
    kind: SyntaxFactKind,
    label: &'static str,
}

fn syntax_fact_count(headers: &[Header]) -> usize {
    headers.len().saturating_mul(2).saturating_add(1)
}

fn required_syntax_fact_count(headers: &[Header]) -> usize {
    headers
        .iter()
        .filter(|header| header.kind != SyntaxFactKind::Import)
        .count()
        .saturating_mul(2)
        .saturating_add(1)
}

fn scan_headers(bytes: &[u8], cancellation: &Cancellation) -> Result<Vec<Header>, AdapterError> {
    std::str::from_utf8(bytes).map_err(|_| failure("invalid-utf8"))?;
    let masked = mask_non_code(bytes, cancellation)?;
    let mut headers = Vec::new();
    let mut start = 0;
    while start < masked.len() {
        cancellation.check()?;
        let end = masked[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(masked.len(), |offset| start + offset);
        if let Some(header) = parse_header(&masked[start..end], start) {
            headers
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            headers.push(header);
        }
        start = end.saturating_add(1);
    }
    Ok(headers)
}

fn parse_header(line: &[u8], start: usize) -> Option<Header> {
    if line.first().is_none_or(|byte| byte.is_ascii_whitespace()) {
        return None;
    }
    let (prefix, kind, label, upper) = if line.starts_with(b"port module ") {
        (12, SyntaxFactKind::Module, "elm.module", true)
    } else if line.starts_with(b"effect module ") {
        (14, SyntaxFactKind::Module, "elm.module", true)
    } else if line.starts_with(b"module ") {
        (7, SyntaxFactKind::Module, "elm.module", true)
    } else if line.starts_with(b"import ") {
        (7, SyntaxFactKind::Import, "elm.import", true)
    } else if line.starts_with(b"type alias ") {
        (
            11,
            SyntaxFactKind::Declaration,
            "elm.type_alias.declaration",
            true,
        )
    } else if line.starts_with(b"type ") {
        (5, SyntaxFactKind::Declaration, "elm.enum.declaration", true)
    } else {
        (
            0,
            SyntaxFactKind::Declaration,
            "elm.function.declaration",
            false,
        )
    };
    let first = *line.get(prefix)?;
    if !first.is_ascii_alphabetic() || first.is_ascii_uppercase() != upper {
        return None;
    }
    let dotted = matches!(kind, SyntaxFactKind::Module | SyntaxFactKind::Import);
    let name_end = line[prefix..]
        .iter()
        .position(|byte| {
            !byte.is_ascii_alphanumeric() && *byte != b'_' && !(dotted && *byte == b'.')
        })
        .map_or(line.len(), |offset| prefix + offset);
    if dotted
        && line[prefix..name_end]
            .split(|byte| *byte == b'.')
            .any(|segment| {
                segment
                    .first()
                    .is_none_or(|first| !first.is_ascii_uppercase())
            })
    {
        return None;
    }
    if name_end == prefix
        || line
            .get(name_end)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
    {
        return None;
    }
    if prefix == 0 && !line[name_end..].contains(&b'=') {
        return None;
    }
    Some(Header {
        line_start: start,
        name_start: start + prefix,
        name_end: start + name_end,
        kind,
        label,
    })
}

fn mask_non_code(bytes: &[u8], cancellation: &Cancellation) -> Result<Vec<u8>, AdapterError> {
    let mut masked = Vec::new();
    masked
        .try_reserve_exact(bytes.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    masked.extend_from_slice(bytes);
    let mut index = 0;
    let mut block_depth = 0_usize;
    let mut line_comment = false;
    let mut quote = None;
    while index < bytes.len() {
        if index % CHECK_INTERVAL == 0 {
            cancellation.check()?;
        }
        let tail = &bytes[index..];
        let length = if line_comment {
            if bytes[index] == b'\n' {
                line_comment = false;
            }
            1
        } else if block_depth > 0 {
            if tail.starts_with(b"{-") {
                block_depth += 1;
                2
            } else if tail.starts_with(b"-}") {
                block_depth -= 1;
                2
            } else {
                1
            }
        } else if let Some((marker, triple)) = quote {
            if tail.starts_with(b"\\") {
                2.min(tail.len())
            } else if triple && tail.starts_with(b"\"\"\"") {
                quote = None;
                3
            } else if !triple && bytes[index] == marker {
                quote = None;
                1
            } else {
                1
            }
        } else if tail.starts_with(b"--") {
            line_comment = true;
            2
        } else if tail.starts_with(b"{-") {
            block_depth = 1;
            2
        } else if tail.starts_with(b"\"\"\"") {
            quote = Some((b'"', true));
            3
        } else if matches!(bytes[index], b'"' | b'\'') {
            quote = Some((bytes[index], false));
            1
        } else {
            index += 1;
            continue;
        };
        for byte in &mut masked[index..index + length] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
        index += length;
    }
    Ok(masked)
}

fn source_span(file: FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    let start = u64::try_from(start).map_err(|_| failure("elm-source-offset-limit"))?;
    let end = u64::try_from(end).map_err(|_| failure("elm-source-offset-limit"))?;
    SourceSpan::new(file, start, end).map_err(|_| failure("elm-source-span-invalid"))
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed Elm syntax label")
}

fn failure(value: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(value).expect("fixed Elm diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_ignore_nested_comments_and_multiline_literals() {
        let source = b"module Main.Page exposing (.. )\n{- type Ghost = Fake {- hidden = 1 -} -}\nimport Html.Attributes\ntype alias Model = { value : Int }\nrender model =\n    \"\"\"\ntype Phantom = False\n\"\"\"\nvalue = 1\n";
        let headers = scan_headers(source, &Cancellation::new()).expect("valid source");
        let names: Vec<_> = headers
            .iter()
            .map(|header| &source[header.name_start..header.name_end])
            .collect();
        assert_eq!(
            names,
            [
                b"Main.Page".as_slice(),
                b"Html.Attributes",
                b"Model",
                b"render",
                b"value"
            ]
        );
    }

    #[test]
    fn headers_keep_exact_utf8_crlf_offsets_and_reject_invalid_source() {
        let source = "module Main exposing (..)\r\n{- Ω -}\r\nvalue = 1\r\n";
        let headers = scan_headers(source.as_bytes(), &Cancellation::new()).expect("valid source");
        assert_eq!(headers.len(), 2);
        for header in headers {
            let name = &source.as_bytes()[header.name_start..header.name_end];
            assert!(name == b"Main" || name == b"value");
            assert_eq!(
                source.as_bytes()[header.line_start],
                if name == b"Main" { b'm' } else { b'v' }
            );
        }
        assert!(scan_headers(b"module Main\n\xff\nvalue = 1\n", &Cancellation::new()).is_err());
    }
}
