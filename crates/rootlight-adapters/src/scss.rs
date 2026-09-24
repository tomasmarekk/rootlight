//! First-party structural scanner for source-backed SCSS declarations.
//! Complete variables, mixins, and functions become identities, while named
//! mixin includes and quoted imports retain source evidence. Import targets and
//! value uses remain bounded.

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
        syntax_fact_count(
            declarations.declarations.len(),
            declarations.scopes.len(),
            declarations.calls.len(),
            declarations.imports.len(),
        )
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
        let fact_count = syntax_fact_count(
            declarations.declarations.len(),
            declarations.scopes.len(),
            declarations.calls.len(),
            declarations.imports.len(),
        )?;
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
        let first_call_id = scope_id(declarations.scopes.len())?;
        for (index, call) in declarations.calls.iter().enumerate() {
            cancellation.check()?;
            let parent = call.scope.map(&scope_id).transpose()?.unwrap_or(1);
            let local_id = first_call_id
                .checked_add(u64::try_from(index).map_err(|_| failure("scss-syntax-node-limit"))?)
                .ok_or_else(|| failure("scss-syntax-node-limit"))?;
            facts.push(SyntaxFact::new(
                local_id,
                Some(parent),
                SyntaxFactKind::Occurrence,
                source_span(file, call.name_start, call.name_end)?,
                call.depth,
                label("scss.mixin.call"),
            ));
        }
        let first_import_id = first_call_id
            .checked_add(
                u64::try_from(declarations.calls.len())
                    .map_err(|_| failure("scss-syntax-node-limit"))?,
            )
            .ok_or_else(|| failure("scss-syntax-node-limit"))?;
        for (index, import) in declarations.imports.iter().enumerate() {
            cancellation.check()?;
            let parent = import.scope.map(&scope_id).transpose()?.unwrap_or(1);
            let local_id = first_import_id
                .checked_add(u64::try_from(index).map_err(|_| failure("scss-syntax-node-limit"))?)
                .ok_or_else(|| failure("scss-syntax-node-limit"))?;
            facts.push(SyntaxFact::new(
                local_id,
                Some(parent),
                SyntaxFactKind::Import,
                source_span(file, import.path_start, import.path_end)?,
                import.depth,
                label("scss.import"),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MixinCall {
    name_start: usize,
    name_end: usize,
    scope: Option<usize>,
    depth: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QuotedImport {
    path_start: usize,
    path_end: usize,
    scope: Option<usize>,
    depth: usize,
}

struct ScanResult {
    declarations: Vec<Declaration>,
    scopes: Vec<BlockScope>,
    calls: Vec<MixinCall>,
    imports: Vec<QuotedImport>,
}

impl ScanResult {
    fn maximum_depth(&self) -> usize {
        self.declarations
            .iter()
            .map(|declaration| declaration.depth + 1)
            .chain(self.scopes.iter().map(|scope| scope.depth))
            .chain(self.calls.iter().map(|call| call.depth))
            .chain(self.imports.iter().map(|import| import.depth))
            .max()
            .unwrap_or(0)
    }
}

fn syntax_fact_count(
    declarations: usize,
    scopes: usize,
    calls: usize,
    imports: usize,
) -> Result<usize, AdapterError> {
    declarations
        .checked_mul(3)
        .and_then(|value| value.checked_add(scopes))
        .and_then(|value| value.checked_add(calls))
        .and_then(|value| value.checked_add(imports))
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
    let mut calls = Vec::<MixinCall>::new();
    let mut imports = Vec::<QuotedImport>::new();
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
                let next_count = syntax_fact_count(next, scopes.len(), calls.len(), imports.len())?;
                if next_count > max_syntax_nodes {
                    return Err(failure("scss-syntax-node-limit"));
                }
                declaration.scope = scope_stack.last().copied();
                declaration.depth = scope_stack.len().saturating_mul(2).saturating_add(1);
                declarations
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                declarations.push(declaration);
            } else if let Some(mut call) = parse_mixin_call(&masked, index) {
                let next = calls
                    .len()
                    .checked_add(1)
                    .ok_or_else(|| failure("scss-syntax-node-limit"))?;
                if syntax_fact_count(declarations.len(), scopes.len(), next, imports.len())?
                    > max_syntax_nodes
                {
                    return Err(failure("scss-syntax-node-limit"));
                }
                call.scope = scope_stack.last().copied();
                call.depth = scope_stack.len().saturating_mul(2).saturating_add(1);
                calls
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                calls.push(call);
            } else {
                let available = max_syntax_nodes.saturating_sub(syntax_fact_count(
                    declarations.len(),
                    scopes.len(),
                    calls.len(),
                    imports.len(),
                )?);
                if let Some(mut paths) = parse_quoted_import_paths(source, index, available)? {
                    for import in &mut paths {
                        import.scope = scope_stack.last().copied();
                        import.depth = scope_stack.len().saturating_mul(2).saturating_add(1);
                    }
                    imports
                        .try_reserve(paths.len())
                        .map_err(|_| SinkError::AllocationFailed)?;
                    imports.extend(paths);
                }
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
                let next_count = syntax_fact_count(
                    declarations.len(),
                    scopes.len() + 1,
                    calls.len(),
                    imports.len(),
                )?;
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
        calls,
        imports,
    })
}

fn parse_quoted_import_paths(
    source: &[u8],
    start: usize,
    maximum_paths: usize,
) -> Result<Option<Vec<QuotedImport>>, AdapterError> {
    let Some(after_keyword) = start.checked_add(b"@import".len()) else {
        return Ok(None);
    };
    if !source
        .get(start..)
        .is_some_and(|tail| tail.starts_with(b"@import"))
        || !source
            .get(after_keyword)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return Ok(None);
    }
    let mut cursor = skip_whitespace(source, after_keyword);
    let mut imports = Vec::new();
    loop {
        let Some(&quote) = source.get(cursor) else {
            return Ok(None);
        };
        if !matches!(quote, b'\'' | b'"') {
            return Ok(None);
        }
        let path_start = cursor + 1;
        cursor = path_start;
        while let Some(&byte) = source.get(cursor) {
            if byte == b'\\' {
                cursor = cursor.saturating_add(2);
            } else if byte == quote {
                break;
            } else {
                cursor += 1;
            }
        }
        if source.get(cursor) != Some(&quote) || cursor == path_start {
            return Ok(None);
        }
        if imports.len() >= maximum_paths {
            return Err(failure("scss-syntax-node-limit"));
        }
        imports
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        imports.push(QuotedImport {
            path_start,
            path_end: cursor,
            scope: None,
            depth: 0,
        });
        cursor = skip_whitespace(source, cursor + 1);
        match source.get(cursor) {
            Some(b',') => cursor = skip_whitespace(source, cursor + 1),
            Some(b';') => return Ok(Some(imports)),
            _ => return Ok(None),
        }
    }
}

fn parse_mixin_call(source: &[u8], start: usize) -> Option<MixinCall> {
    let after_keyword = start.checked_add(b"@include".len())?;
    if !source.get(start..)?.starts_with(b"@include")
        || !source.get(after_keyword)?.is_ascii_whitespace()
    {
        return None;
    }
    let name_start = skip_whitespace(source, after_keyword);
    let mut name_end = identifier_end(source, name_start)?;
    if source.get(name_start..)?.starts_with(b"--") {
        return None;
    }
    if source.get(name_end) == Some(&b'.') {
        name_end = identifier_end(source, name_end.checked_add(1)?)?;
    }
    let mut cursor = skip_whitespace(source, name_end);
    if source.get(cursor) == Some(&b'(') {
        cursor = balanced_end(source, cursor, b'(', b')')?;
        cursor = skip_whitespace(source, cursor);
    }
    if source.get(cursor..)?.starts_with(b"using")
        && source.get(cursor + b"using".len())?.is_ascii_whitespace()
    {
        cursor = skip_whitespace(source, cursor + b"using".len());
        cursor = balanced_end(source, cursor, b'(', b')')?;
        cursor = skip_whitespace(source, cursor);
    }
    match source.get(cursor)? {
        b';' => {}
        b'{' => {
            balanced_end(source, cursor, b'{', b'}')?;
        }
        _ => return None,
    }
    Some(MixinCall {
        name_start,
        name_end,
        scope: None,
        depth: 0,
    })
}

fn balanced_end(source: &[u8], start: usize, open: u8, close: u8) -> Option<usize> {
    if source.get(start) != Some(&open) {
        return None;
    }
    let mut depth = 0_usize;
    for (offset, &byte) in source.get(start..)?.iter().enumerate() {
        if byte == open {
            depth = depth.checked_add(1)?;
        } else if byte == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return start.checked_add(offset)?.checked_add(1);
            }
        }
    }
    None
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

    #[test]
    fn captures_complete_named_includes_and_ignores_incomplete_or_masked_ones() {
        let source = b"@mixin card($radius) { border-radius: $radius; }\n.panel { @include card(2px); @include theme.card using ($value) { color: $value; } content: \"@include ghost;\"; /* @include phantom; */ }\n";
        let result = scan_source(source, 64, &Cancellation::new()).unwrap();
        assert_eq!(result.calls.len(), 2);
        assert_eq!(
            result
                .calls
                .iter()
                .map(|call| &source[call.name_start..call.name_end])
                .collect::<Vec<_>>(),
            [&b"card"[..], &b"theme.card"[..]]
        );
        assert_eq!(result.calls[0].scope, Some(1));
        assert_eq!(result.calls[1].scope, Some(1));
        for source in [
            &b"@include card"[..],
            &b"@include card("[..],
            &b"@include card nonsense;"[..],
            &b"@include card#{dynamic};"[..],
        ] {
            assert!(
                scan_source(source, 64, &Cancellation::new())
                    .unwrap()
                    .calls
                    .is_empty()
            );
        }
        assert!(scan_source(b"@include card;", 1, &Cancellation::new()).is_err());
    }

    #[test]
    fn captures_each_quoted_import_path_without_comment_or_string_lookalikes() {
        let source = b"@import \"base/_mixins.scss\";\n@import 'colors.scss', \"layout.scss\";\n.note { content: \"@import ghost.scss;\"; /* @import 'phantom.scss'; */ }\n";
        let result = scan_source(source, 64, &Cancellation::new()).unwrap();
        assert_eq!(result.imports.len(), 3);
        assert_eq!(
            result
                .imports
                .iter()
                .map(|import| &source[import.path_start..import.path_end])
                .collect::<Vec<_>>(),
            [
                &b"base/_mixins.scss"[..],
                &b"colors.scss"[..],
                &b"layout.scss"[..],
            ]
        );
        for source in [
            &b"@import \"unfinished;"[..],
            &b"@import \"path.scss\""[..],
            &b"@import url(\"sheet.css\");"[..],
        ] {
            assert!(
                scan_source(source, 64, &Cancellation::new())
                    .unwrap()
                    .imports
                    .is_empty()
            );
        }
        assert!(scan_source(b"@import \"path.scss\";", 1, &Cancellation::new()).is_err());
    }
}
