//! First-party structural scanner for source-backed SCSS declarations.
//! Only complete variables, mixins, and functions become identities;
//! selectors, imports, and value uses remain bounded.

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

/// Bounded first-party parser for complete SCSS declarations.
pub struct ScssParseProvider {
    capabilities: ParseCapabilities,
}

impl ScssParseProvider {
    /// Returns the content identity of this first-party frontend.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("scss.rs"))
    }

    /// Creates a parser with the caller's source and syntax-node ceilings.
    ///
    /// # Errors
    /// Returns [`DescriptorError`] when a ceiling is zero.
    ///
    /// # Panics
    /// Panics only if the fixed `scss` label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        let language = LanguageId::new("scss").expect("fixed SCSS language label");
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

    /// Counts the same syntax identities that parsing will emit.
    ///
    /// # Errors
    /// Returns [`AdapterError`] for invalid UTF-8, cancellation, or a syntax ceiling.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        let declarations = scan_source(
            request.source().bytes(),
            request
                .limits()
                .max_syntax_nodes()
                .min(self.capabilities.max_syntax_nodes()),
            cancellation,
        )?;
        if declarations.maximum_depth()
            > request
                .limits()
                .max_syntax_depth()
                .min(self.capabilities.max_syntax_depth())
        {
            return Err(failure("scss-syntax-depth-limit"));
        }
        syntax_fact_count(declarations.declarations.len(), declarations.scopes.len())
    }
}

impl ParseProvider for ScssParseProvider {
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
        let declarations = scan_source(
            source.bytes(),
            request
                .limits()
                .max_syntax_nodes()
                .min(self.capabilities.max_syntax_nodes()),
            cancellation,
        )?;
        let maximum_depth = declarations.maximum_depth();
        if maximum_depth
            > request
                .limits()
                .max_syntax_depth()
                .min(self.capabilities.max_syntax_depth())
        {
            return Err(failure("scss-syntax-depth-limit"));
        }
        let fact_count =
            syntax_fact_count(declarations.declarations.len(), declarations.scopes.len())?;
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
            label("scss.source_file"),
        ));
        let scope_id = |index: usize| -> Result<u64, AdapterError> {
            u64::try_from(declarations.declarations.len())
                .ok()
                .and_then(|count| count.checked_mul(3))
                .and_then(|count| count.checked_add(2))
                .and_then(|first| first.checked_add(u64::try_from(index).ok()?))
                .ok_or_else(|| failure("scss-syntax-node-limit"))
        };
        for (index, declaration) in declarations.declarations.iter().enumerate() {
            cancellation.check()?;
            let local_id = u64::try_from(index)
                .ok()
                .and_then(|value| value.checked_mul(3))
                .and_then(|value| value.checked_add(2))
                .ok_or_else(|| failure("scss-syntax-node-limit"))?;
            let name = source_span(file, declaration.name_start, declaration.name_end)?;
            let parent = declaration.scope.map(&scope_id).transpose()?.unwrap_or(1);
            let depth = declaration.depth;
            facts.push(SyntaxFact::new(
                local_id,
                Some(parent),
                SyntaxFactKind::Declaration,
                source_span(file, declaration.start, declaration.end)?,
                depth,
                label(declaration.kind.label()),
            ));
            facts.push(SyntaxFact::new(
                local_id + 1,
                Some(local_id),
                SyntaxFactKind::Signature,
                name,
                depth + 1,
                label("scss.name.signature"),
            ));
            facts.push(SyntaxFact::new(
                local_id + 2,
                Some(local_id),
                SyntaxFactKind::Occurrence,
                name,
                depth + 1,
                label("scss.identifier.definition"),
            ));
        }
        for (index, scope) in declarations.scopes.iter().enumerate() {
            cancellation.check()?;
            let parent = scope
                .owner
                .map(|declaration| {
                    u64::try_from(declaration)
                        .ok()
                        .and_then(|index| index.checked_mul(3))
                        .and_then(|index| index.checked_add(2))
                        .ok_or_else(|| failure("scss-syntax-node-limit"))
                })
                .transpose()?
                .or(scope.parent.map(scope_id).transpose()?)
                .unwrap_or(1);
            facts.push(SyntaxFact::new(
                scope_id(index)?,
                Some(parent),
                SyntaxFactKind::Scope,
                source_span(file, scope.start, scope.end)?,
                scope.depth,
                label("scss.block.scope"),
            ));
        }
        facts.sort_by_key(|fact| (fact.span().start_byte(), fact.depth()));
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
                    declarations.declarations.len(),
                    declarations.declarations.len(),
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
                maximum_depth,
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeclarationKind {
    Variable,
    Mixin,
    Function,
}

impl DeclarationKind {
    fn label(self) -> &'static str {
        match self {
            Self::Variable => "scss.variable.declaration",
            Self::Mixin => "scss.mixin.declaration",
            Self::Function => "scss.function.declaration",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Declaration {
    kind: DeclarationKind,
    start: usize,
    end: usize,
    name_start: usize,
    name_end: usize,
    opening: Option<usize>,
    scope: Option<usize>,
    depth: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BlockScope {
    start: usize,
    end: usize,
    parent: Option<usize>,
    owner: Option<usize>,
    depth: usize,
}

struct ScanResult {
    declarations: Vec<Declaration>,
    scopes: Vec<BlockScope>,
}

impl ScanResult {
    fn maximum_depth(&self) -> usize {
        self.declarations
            .iter()
            .map(|declaration| declaration.depth + 1)
            .chain(self.scopes.iter().map(|scope| scope.depth))
            .max()
            .unwrap_or(0)
    }
}

fn syntax_fact_count(declarations: usize, scopes: usize) -> Result<usize, AdapterError> {
    declarations
        .checked_mul(3)
        .and_then(|value| value.checked_add(scopes))
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| failure("scss-syntax-node-limit"))
}

fn scan_source(
    source: &[u8],
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<ScanResult, AdapterError> {
    std::str::from_utf8(source).map_err(|_| failure("invalid-utf8"))?;
    let masked = mask_non_code(source, cancellation)?;
    let mut declarations = Vec::new();
    let mut scopes = Vec::<BlockScope>::new();
    let mut scope_stack = Vec::<usize>::new();
    let mut interpolation_depth = 0_usize;
    let mut parentheses = 0_usize;
    let mut brackets = 0_usize;
    let mut statement_start = true;
    for (index, &byte) in masked.iter().enumerate() {
        if index.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if interpolation_depth == 0
            && parentheses == 0
            && brackets == 0
            && statement_start
            && !byte.is_ascii_whitespace()
        {
            if let Some(mut declaration) = parse_declaration(&masked, source, index) {
                let next = declarations
                    .len()
                    .checked_add(1)
                    .ok_or_else(|| failure("scss-syntax-node-limit"))?;
                let next_count = syntax_fact_count(next, scopes.len())?;
                if next_count > max_syntax_nodes {
                    return Err(failure("scss-syntax-node-limit"));
                }
                declaration.scope = scope_stack.last().copied();
                declaration.depth = scope_stack.len().saturating_mul(2).saturating_add(1);
                declarations
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                declarations.push(declaration);
            }
            statement_start = false;
        }
        match byte {
            b'(' => parentheses = parentheses.saturating_add(1),
            b')' => parentheses = parentheses.saturating_sub(1),
            b'[' => brackets = brackets.saturating_add(1),
            b']' => brackets = brackets.saturating_sub(1),
            b'{' if masked.get(index.wrapping_sub(1)) == Some(&b'#')
                || interpolation_depth != 0 =>
            {
                interpolation_depth = interpolation_depth.saturating_add(1);
            }
            b'{' if parentheses == 0 && brackets == 0 => {
                let next_count = syntax_fact_count(declarations.len(), scopes.len() + 1)?;
                if next_count > max_syntax_nodes {
                    return Err(failure("scss-syntax-node-limit"));
                }
                let parent = scope_stack.last().copied();
                let owner = declarations
                    .last()
                    .is_some_and(|declaration| declaration.opening == Some(index))
                    .then(|| declarations.len() - 1);
                let depth = scope_stack.len().saturating_mul(2).saturating_add(1)
                    + usize::from(owner.is_some());
                scopes
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let scope_index = scopes.len();
                scopes.push(BlockScope {
                    start: index,
                    end: source.len(),
                    parent,
                    owner,
                    depth,
                });
                scope_stack
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                scope_stack.push(scope_index);
                statement_start = true;
            }
            b'}' if interpolation_depth != 0 => {
                interpolation_depth -= 1;
            }
            b'}' if parentheses == 0 && brackets == 0 => {
                if let Some(scope_index) = scope_stack.pop() {
                    scopes[scope_index].end = index + 1;
                }
                statement_start = true;
            }
            b';' if parentheses == 0 && brackets == 0 && interpolation_depth == 0 => {
                statement_start = true;
            }
            _ => {}
        }
    }
    cancellation.check()?;
    Ok(ScanResult {
        declarations,
        scopes,
    })
}

fn parse_declaration(masked: &[u8], source: &[u8], start: usize) -> Option<Declaration> {
    let (&first, _) = masked.get(start..)?.split_first()?;
    let (kind, name_start) = if first == b'$' {
        (DeclarationKind::Variable, start.checked_add(1)?)
    } else {
        let (kind, keyword_len) = if masked.get(start..)?.starts_with(b"@mixin") {
            (DeclarationKind::Mixin, 6)
        } else if masked.get(start..)?.starts_with(b"@function") {
            (DeclarationKind::Function, 9)
        } else {
            return None;
        };
        let after_keyword = start.checked_add(keyword_len)?;
        if !masked.get(after_keyword)?.is_ascii_whitespace() {
            return None;
        }
        (kind, skip_whitespace(masked, after_keyword))
    };
    let name_end = identifier_end(masked, name_start)?;
    let after_name = skip_whitespace(masked, name_end);
    let (end, opening) = match kind {
        DeclarationKind::Variable => {
            if masked.get(after_name) != Some(&b':') {
                return None;
            }
            let end = variable_end(masked, after_name + 1)?;
            source
                .get(after_name + 1..end - 1)?
                .iter()
                .any(|byte| !byte.is_ascii_whitespace())
                .then_some((end, None))?
        }
        DeclarationKind::Mixin | DeclarationKind::Function => {
            let mut opening = after_name;
            if masked.get(opening) == Some(&b'(') {
                let mut parentheses = 1_usize;
                opening += 1;
                while let Some(&byte) = masked.get(opening) {
                    match byte {
                        b'(' => parentheses = parentheses.checked_add(1)?,
                        b')' => {
                            parentheses = parentheses.checked_sub(1)?;
                            if parentheses == 0 {
                                opening += 1;
                                break;
                            }
                        }
                        b'{' | b'}' if parentheses == 1 => return None,
                        _ => {}
                    }
                    opening += 1;
                }
                if parentheses != 0 {
                    return None;
                }
                opening = skip_whitespace(masked, opening);
            } else if kind == DeclarationKind::Function {
                return None;
            }
            if masked.get(opening) != Some(&b'{') {
                return None;
            }
            let mut braces = 1_usize;
            let mut ending = None;
            for (offset, &byte) in masked.get(opening + 1..)?.iter().enumerate() {
                match byte {
                    b'{' => braces = braces.checked_add(1)?,
                    b'}' => {
                        braces = braces.checked_sub(1)?;
                        if braces == 0 {
                            ending = Some(opening + offset + 2);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            (ending?, Some(opening))
        }
    };
    Some(Declaration {
        kind,
        start,
        end,
        name_start,
        name_end,
        opening,
        scope: None,
        depth: 0,
    })
}

fn variable_end(source: &[u8], start: usize) -> Option<usize> {
    let mut parentheses = 0_usize;
    let mut brackets = 0_usize;
    let mut interpolation = 0_usize;
    for (offset, &byte) in source.get(start..)?.iter().enumerate() {
        let index = start + offset;
        match byte {
            b'(' => parentheses = parentheses.checked_add(1)?,
            b')' => parentheses = parentheses.checked_sub(1)?,
            b'[' => brackets = brackets.checked_add(1)?,
            b']' => brackets = brackets.checked_sub(1)?,
            b'{' if source.get(index.wrapping_sub(1)) == Some(&b'#') || interpolation != 0 => {
                interpolation = interpolation.checked_add(1)?;
            }
            b'}' if interpolation != 0 => interpolation -= 1,
            b'{' | b'}' if parentheses == 0 && brackets == 0 => return None,
            b';' if parentheses == 0 && brackets == 0 && interpolation == 0 => {
                return Some(index + 1);
            }
            _ => {}
        }
    }
    None
}

fn identifier_end(source: &[u8], start: usize) -> Option<usize> {
    let first = *source.get(start)?;
    if !first.is_ascii_alphabetic() && !matches!(first, b'_' | b'-') {
        return None;
    }
    let mut end = start + 1;
    while source
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        end += 1;
    }
    Some(end)
}

fn skip_whitespace(source: &[u8], mut index: usize) -> usize {
    while source.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index
}

fn mask_non_code(source: &[u8], cancellation: &Cancellation) -> Result<Vec<u8>, AdapterError> {
    let mut masked = source.to_vec();
    let mut index = 0_usize;
    while index < source.len() {
        if index.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        let length = if source
            .get(index..)
            .is_some_and(|tail| tail.starts_with(b"//"))
        {
            source[index..]
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(source.len() - index)
        } else if source
            .get(index..)
            .is_some_and(|tail| tail.starts_with(b"/*"))
        {
            source[index + 2..]
                .windows(2)
                .position(|pair| pair == b"*/")
                .map_or(source.len() - index, |offset| offset + 4)
        } else if matches!(source[index], b'\'' | b'"') {
            let quote = source[index];
            let mut end = index + 1;
            while let Some(&byte) = source.get(end) {
                if byte == b'\\' {
                    end = end.saturating_add(2);
                } else {
                    end += 1;
                    if byte == quote {
                        break;
                    }
                }
            }
            end.min(source.len()) - index
        } else {
            index += 1;
            continue;
        };
        let end = index + length;
        for byte in masked
            .get_mut(index..end)
            .ok_or_else(|| failure("scss-source-span-invalid"))?
        {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
        index = end;
    }
    cancellation.check()?;
    Ok(masked)
}

fn source_span(file: FileId, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    let start = u64::try_from(start).map_err(|_| failure("scss-source-offset-limit"))?;
    let end = u64::try_from(end).map_err(|_| failure("scss-source-offset-limit"))?;
    SourceSpan::new(file, start, end).map_err(|_| failure("scss-source-span-invalid"))
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed SCSS syntax label")
}

fn failure(value: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(value).expect("fixed SCSS diagnostic code"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_nested_declarations_without_comment_or_string_lookalikes() {
        let source = b"$accent: #123;\n@mixin card($r) { border-radius: $r; }\n@function scale($x) { @return $x * 2; }\n/* $ghost: red; */\n.note { content: \"@mixin phantom {}\"; $nested: blue; }\n";
        let result = scan_source(source, 64, &Cancellation::new()).unwrap();
        assert_eq!(result.declarations.len(), 4);
        assert_eq!(
            result
                .declarations
                .iter()
                .map(|declaration| (
                    &source[declaration.name_start..declaration.name_end],
                    declaration.kind
                ))
                .collect::<Vec<_>>(),
            [
                (&b"accent"[..], DeclarationKind::Variable),
                (&b"card"[..], DeclarationKind::Mixin),
                (&b"scale"[..], DeclarationKind::Function),
                (&b"nested"[..], DeclarationKind::Variable),
            ]
        );
        assert_eq!(result.scopes.len(), 3);
        assert_eq!(result.declarations[3].scope, Some(2));
    }

    #[test]
    fn rejects_incomplete_and_oversized_declarations() {
        for source in [
            &b"$x: value"[..],
            &b"@mixin card($x) {"[..],
            &b"@function scale($x);"[..],
            &b"@mixin-card { }"[..],
            &b"@mixin card nonsense { }"[..],
            &b"@function scale { }"[..],
            &b"$empty: ;"[..],
        ] {
            assert!(
                scan_source(source, 64, &Cancellation::new())
                    .unwrap()
                    .declarations
                    .is_empty()
            );
        }
        assert!(scan_source(b"$x: 1;", 3, &Cancellation::new()).is_err());
    }

    #[test]
    fn retains_multiline_and_string_values_without_confusing_interpolation_with_blocks() {
        let source = b"$label: \"name\";\n$map: (\n  key: (one, two),\n);\n@mixin outer {\n  $value: #{1 + 2};\n  @if true { $value: 4; }\n}\n";
        let result = scan_source(source, 64, &Cancellation::new()).unwrap();
        assert_eq!(result.declarations.len(), 5);
        assert_eq!(result.scopes.len(), 2);
        assert_eq!(result.declarations[3].scope, Some(0));
        assert_eq!(result.declarations[4].scope, Some(1));
    }
}
