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
        let source = scan_source(
            request.source().bytes(),
            request
                .limits()
                .max_syntax_nodes()
                .min(self.capabilities.max_syntax_nodes()),
            cancellation,
        )?;
        Ok(required_syntax_fact_count(&source))
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
            label("elm.source_file"),
        ));
        let mut next_id = 2_u64;
        let mut constructor_index = 0_usize;
        for (index, header) in parsed.headers.iter().enumerate() {
            cancellation.check()?;
            let local_id = next_id;
            next_id = next_id
                .checked_add(2)
                .ok_or_else(|| failure("elm-syntax-node-limit"))?;
            let end = parsed
                .headers
                .get(index + 1)
                .map_or(source.bytes().len(), |next| next.line_start);
            let span = if header.kind == SyntaxFactKind::Import {
                source_span(file, header.name_start, header.name_end)?
            } else {
                source_span(file, header.line_start, end)?
            };
            facts.push(SyntaxFact::new(
                local_id,
                Some(1),
                header.kind,
                span,
                1,
                label(header.label),
            ));
            if header.kind != SyntaxFactKind::Import {
                facts.push(SyntaxFact::new(
                    local_id + 1,
                    Some(local_id),
                    SyntaxFactKind::Occurrence,
                    source_span(file, header.name_start, header.name_end)?,
                    2,
                    label("elm.name.definition"),
                ));
            }
            while let Some(constructor) = parsed.constructors.get(constructor_index)
                && constructor.owner_index == index
            {
                let constructor_id = next_id;
                next_id = next_id
                    .checked_add(2)
                    .ok_or_else(|| failure("elm-syntax-node-limit"))?;
                facts.push(SyntaxFact::new(
                    constructor_id,
                    Some(local_id),
                    SyntaxFactKind::Declaration,
                    source_span(file, constructor.name_start, constructor.span_end)?,
                    2,
                    label("elm.constructor.declaration"),
                ));
                facts.push(SyntaxFact::new(
                    constructor_id + 1,
                    Some(constructor_id),
                    SyntaxFactKind::Occurrence,
                    source_span(file, constructor.name_start, constructor.name_end)?,
                    3,
                    label("elm.name.definition"),
                ));
                constructor_index += 1;
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
        let declaration_count = parsed
            .headers
            .iter()
            .filter(|header| header.kind != SyntaxFactKind::Import)
            .count()
            .saturating_add(parsed.constructors.len());
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
                    declaration_count,
                    declaration_count,
                    0,
                )?,
            ],
        )?;
        Ok(ParseReport::new(
            coverage,
            ResourceUsage::new(
                source_bytes,
                facts.len(),
                facts.len(),
                facts.iter().map(SyntaxFact::depth).max().unwrap_or(0),
                None,
                usage,
            ),
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

#[derive(Clone, Copy)]
struct Annotation {
    line_start: usize,
    name_start: usize,
    name_end: usize,
}

#[derive(Clone, Copy)]
struct Constructor {
    owner_index: usize,
    name_start: usize,
    name_end: usize,
    span_end: usize,
}

struct ParsedSource {
    headers: Vec<Header>,
    constructors: Vec<Constructor>,
}

struct ConstructorCollector<'a> {
    masked: &'a [u8],
    cancellation: &'a Cancellation,
    max_items: usize,
    constructors: Vec<Constructor>,
}

fn syntax_fact_count(source: &ParsedSource) -> usize {
    source
        .headers
        .iter()
        .fold(1_usize, |count, header| {
            count.saturating_add(if header.kind == SyntaxFactKind::Import {
                1
            } else {
                2
            })
        })
        .saturating_add(source.constructors.len().saturating_mul(2))
}

fn required_syntax_fact_count(source: &ParsedSource) -> usize {
    source
        .headers
        .iter()
        .filter(|header| header.kind != SyntaxFactKind::Import)
        .count()
        .saturating_add(source.constructors.len())
        .saturating_mul(2)
        .saturating_add(1)
}

fn scan_source(
    bytes: &[u8],
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<ParsedSource, AdapterError> {
    if max_syntax_nodes == 0 {
        return Err(failure("elm-syntax-node-limit"));
    }
    std::str::from_utf8(bytes).map_err(|_| failure("invalid-utf8"))?;
    let masked = mask_non_code(bytes, cancellation)?;
    let mut headers = Vec::new();
    let mut header_fact_count = 1_usize;
    let mut start = 0;
    let mut annotation: Option<Annotation> = None;
    while start < masked.len() {
        cancellation.check()?;
        let end = masked[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(masked.len(), |offset| start + offset);
        let line = &masked[start..end];
        if let Some(mut header) = parse_header(line, start) {
            header_fact_count = header_fact_count
                .checked_add(if header.kind == SyntaxFactKind::Import {
                    1
                } else {
                    2
                })
                .filter(|count| *count <= max_syntax_nodes)
                .ok_or_else(|| failure("elm-syntax-node-limit"))?;
            if header.label == "elm.function.declaration"
                && let Some(prior) = annotation
                && masked[prior.name_start..prior.name_end]
                    == masked[header.name_start..header.name_end]
            {
                header.line_start = prior.line_start;
            }
            annotation = None;
            headers
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            headers.push(header);
        } else if let Some(candidate) = parse_annotation(line, start) {
            annotation = Some(candidate);
        } else if line.first().is_some_and(|byte| !byte.is_ascii_whitespace()) {
            annotation = None;
        }
        start = end.saturating_add(1);
    }
    let mut collector = ConstructorCollector {
        masked: &masked,
        cancellation,
        max_items: (max_syntax_nodes - header_fact_count) / 2,
        constructors: Vec::new(),
    };
    for (owner_index, header) in headers.iter().enumerate() {
        cancellation.check()?;
        if header.label != "elm.enum.declaration" {
            continue;
        }
        let end = headers
            .get(owner_index + 1)
            .map_or(masked.len(), |next| next.line_start);
        collector.scan(header.name_end, end, owner_index)?;
    }
    Ok(ParsedSource {
        headers,
        constructors: collector.constructors,
    })
}

impl ConstructorCollector<'_> {
    fn scan(&mut self, start: usize, end: usize, owner_index: usize) -> Result<(), AdapterError> {
        let Some(equals) = self.masked[start..end]
            .iter()
            .position(|byte| *byte == b'=')
            .map(|offset| start + offset)
        else {
            return Ok(());
        };
        let scan_start = equals + 1;
        let mut segment_start = scan_start;
        let mut depth = 0_usize;
        for index in scan_start..end {
            if index % CHECK_INTERVAL == 0 {
                self.cancellation.check()?;
            }
            match self.masked[index] {
                b'(' | b'[' | b'{' => depth = depth.saturating_add(1),
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'|' if depth == 0 => {
                    self.capture(segment_start, index, owner_index)?;
                    segment_start = index + 1;
                }
                _ => {}
            }
        }
        self.capture(segment_start, end, owner_index)
    }

    fn capture(
        &mut self,
        start: usize,
        end: usize,
        owner_index: usize,
    ) -> Result<(), AdapterError> {
        let segment = &self.masked[start..end];
        let Some(prefix) = segment.iter().position(|byte| !byte.is_ascii_whitespace()) else {
            return Ok(());
        };
        let name_start = start + prefix;
        if !self.masked[name_start].is_ascii_uppercase() {
            return Ok(());
        }
        let name_end = self.masked[name_start..end]
            .iter()
            .position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
            .map_or(end, |length| name_start + length);
        if self.masked[name_end..end]
            .first()
            .is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            return Ok(());
        }
        if self.constructors.len() >= self.max_items {
            return Err(failure("elm-syntax-node-limit"));
        }
        let span_end = segment
            .iter()
            .rposition(|byte| !byte.is_ascii_whitespace())
            .map_or(name_end, |offset| start + offset + 1);
        self.constructors
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.constructors.push(Constructor {
            owner_index,
            name_start,
            name_end,
            span_end,
        });
        Ok(())
    }
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
    } else if line.starts_with(b"port ") {
        (
            5,
            SyntaxFactKind::Declaration,
            "elm.port.declaration",
            false,
        )
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
    if label == "elm.port.declaration"
        && line[name_end..]
            .iter()
            .find(|byte| !byte.is_ascii_whitespace())
            != Some(&b':')
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

fn parse_annotation(line: &[u8], start: usize) -> Option<Annotation> {
    let first = *line.first()?;
    if !first.is_ascii_lowercase() {
        return None;
    }
    let name_end = line
        .iter()
        .position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
        .unwrap_or(line.len());
    if line[name_end..]
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        != Some(&b':')
    {
        return None;
    }
    Some(Annotation {
        line_start: start,
        name_start: start,
        name_end: start + name_end,
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
        let parsed = scan_source(source, usize::MAX, &Cancellation::new()).expect("valid source");
        let names: Vec<_> = parsed
            .headers
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
        let parsed =
            scan_source(source.as_bytes(), usize::MAX, &Cancellation::new()).expect("valid source");
        assert_eq!(parsed.headers.len(), 2);
        for header in parsed.headers {
            let name = &source.as_bytes()[header.name_start..header.name_end];
            assert!(name == b"Main" || name == b"value");
            assert_eq!(
                source.as_bytes()[header.line_start],
                if name == b"Main" { b'm' } else { b'v' }
            );
        }
        assert!(
            scan_source(
                b"module Main\n\xff\nvalue = 1\n",
                usize::MAX,
                &Cancellation::new()
            )
            .is_err()
        );
    }

    #[test]
    fn import_names_have_exact_spans_and_one_bounded_fact() {
        let source =
            b"module Main exposing (main)\nimport Shared exposing (value)\nmain = Shared.value\n";
        let parsed = scan_source(source, 6, &Cancellation::new()).expect("bounded Elm source");
        let import = parsed
            .headers
            .iter()
            .find(|header| header.kind == SyntaxFactKind::Import)
            .expect("Elm import exists");
        assert_eq!(&source[import.name_start..import.name_end], b"Shared");
        assert_eq!(syntax_fact_count(&parsed), 6);
        assert_eq!(required_syntax_fact_count(&parsed), 5);
        assert!(scan_source(source, 5, &Cancellation::new()).is_err());
    }

    #[test]
    fn custom_type_constructors_are_bounded_to_their_parent_type() {
        let source = b"module Demo exposing (..)\ntype State\n    = Ready\n    | Failed (List String) {- | Ghost -}\n    | Loaded String\ntype alias Model = { x : Int }\n";
        let parsed = scan_source(source, usize::MAX, &Cancellation::new()).expect("valid source");
        let names: Vec<_> = parsed
            .constructors
            .iter()
            .map(|constructor| &source[constructor.name_start..constructor.name_end])
            .collect();
        assert_eq!(names, [b"Ready".as_slice(), b"Failed", b"Loaded"]);
        assert!(
            parsed
                .constructors
                .iter()
                .all(|constructor| constructor.owner_index == 1)
        );
        assert_eq!(required_syntax_fact_count(&parsed), 13);
        assert_eq!(syntax_fact_count(&parsed), 13);
        assert!(scan_source(source, 12, &Cancellation::new()).is_err());
    }

    #[test]
    fn ports_and_matching_type_annotations_preserve_source_boundaries() {
        let source = b"port module Demo exposing (..)\nport send : String -> Cmd msg\nrender : Model -> String\nrender model = \"ready\"\norphan : Int\nother = 1\n";
        let parsed = scan_source(source, usize::MAX, &Cancellation::new()).expect("valid source");
        let names: Vec<_> = parsed
            .headers
            .iter()
            .map(|header| &source[header.name_start..header.name_end])
            .collect();
        assert_eq!(names, [b"Demo".as_slice(), b"send", b"render", b"other"]);
        assert_eq!(parsed.headers[1].label, "elm.port.declaration");
        assert!(source[parsed.headers[2].line_start..].starts_with(b"render :"));
        assert!(source[parsed.headers[3].line_start..].starts_with(b"other ="));
        assert_eq!(required_syntax_fact_count(&parsed), 9);
        assert!(scan_source(source, 8, &Cancellation::new()).is_err());
    }
}
