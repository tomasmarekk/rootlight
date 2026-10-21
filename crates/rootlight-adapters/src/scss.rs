//! First-party structural scanner for source-backed SCSS declarations.
//! Static variables, mixins, functions, keyframes, and balanced style-rule
//! blocks become identities.
//! Named mixin includes, simple selector extensions, and quoted module loads
//! retain source evidence; their targets and value uses remain bounded.

use crate::source_projection;

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

    /// Creates a bounded UTF-8 and BOM-backed UTF-16 SCSS scanner.
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
            vec![
                EncodingId::utf8(),
                EncodingId::new("utf-16le").expect("fixed UTF-16LE label"),
                EncodingId::new("utf-16be").expect("fixed UTF-16BE label"),
            ],
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
        let view = self.source_view(request, cancellation)?;
        let declarations = scan_source(
            view.text().as_bytes(),
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
        declarations.fact_count()
    }

    fn source_view<'a>(
        &self,
        request: &'a ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<source_projection::View<'a>, AdapterError> {
        cancellation.check()?;
        if request.language().as_str() != "scss"
            || !matches!(
                request.encoding().as_str(),
                "utf-8" | "utf-16le" | "utf-16be"
            )
            || !request.included_ranges().is_empty()
        {
            return Err(failure("scss-source-context-unsupported"));
        }
        let view = source_projection::View::new(
            request.source().bytes(),
            request.encoding().as_str(),
            self.capabilities
                .max_source_bytes()
                .min(request.limits().max_source_bytes()),
            request.limits().max_reported_memory_bytes(),
            cancellation,
        )?;
        if view.text().contains('\0') {
            return Err(failure("scss-invalid-source-nul"));
        }
        Ok(view)
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
        let view = self.source_view(request, cancellation)?;
        let declarations = scan_source(
            view.text().as_bytes(),
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
        let fact_count = declarations.fact_count()?;
        let file = source.source_ref().span().file();
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(fact_count)
            .map_err(|_| SinkError::AllocationFailed)?;
        facts.push(SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source_span(file, 0, view.text().len())?,
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
            let parent = declaration.scope.map(scope_id).transpose()?.unwrap_or(1);
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
                label(declaration.kind.signature_label()),
            ));
            facts.push(SyntaxFact::new(
                local_id + 2,
                Some(local_id),
                SyntaxFactKind::Occurrence,
                name,
                depth + 1,
                label(declaration.kind.definition_label()),
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
        let first_parameter_id = scope_id(declarations.scopes.len())?;
        for (index, parameter) in declarations.parameters.iter().enumerate() {
            cancellation.check()?;
            let local_id = first_parameter_id
                .checked_add(
                    u64::try_from(index)
                        .ok()
                        .and_then(|index| index.checked_mul(3))
                        .ok_or_else(|| failure("scss-syntax-node-limit"))?,
                )
                .ok_or_else(|| failure("scss-syntax-node-limit"))?;
            let parent = u64::try_from(parameter.owner)
                .ok()
                .and_then(|owner| owner.checked_mul(3))
                .and_then(|owner| owner.checked_add(2))
                .ok_or_else(|| failure("scss-syntax-node-limit"))?;
            let name = source_span(file, parameter.name_start, parameter.name_end)?;
            facts.push(SyntaxFact::new(
                local_id,
                Some(parent),
                SyntaxFactKind::Declaration,
                source_span(file, parameter.start, parameter.end)?,
                parameter.depth,
                label("scss.parameter.declaration"),
            ));
            facts.push(SyntaxFact::new(
                local_id + 1,
                Some(local_id),
                SyntaxFactKind::Signature,
                name,
                parameter.depth + 1,
                label("scss.name.signature"),
            ));
            facts.push(SyntaxFact::new(
                local_id + 2,
                Some(local_id),
                SyntaxFactKind::Occurrence,
                name,
                parameter.depth + 1,
                label("scss.identifier.definition"),
            ));
        }
        let first_call_id = first_parameter_id
            .checked_add(
                u64::try_from(declarations.parameters.len())
                    .ok()
                    .and_then(|count| count.checked_mul(3))
                    .ok_or_else(|| failure("scss-syntax-node-limit"))?,
            )
            .ok_or_else(|| failure("scss-syntax-node-limit"))?;
        for (index, call) in declarations.calls.iter().enumerate() {
            cancellation.check()?;
            let parent = call.scope.map(scope_id).transpose()?.unwrap_or(1);
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
            let parent = import.scope.map(scope_id).transpose()?.unwrap_or(1);
            let local_id = first_import_id
                .checked_add(u64::try_from(index).map_err(|_| failure("scss-syntax-node-limit"))?)
                .ok_or_else(|| failure("scss-syntax-node-limit"))?;
            facts.push(SyntaxFact::new(
                local_id,
                Some(parent),
                SyntaxFactKind::Import,
                source_span(file, import.path_start, import.path_end)?,
                import.depth,
                label(import.kind.label()),
            ));
        }
        let first_extend_id = first_import_id
            .checked_add(
                u64::try_from(declarations.imports.len())
                    .map_err(|_| failure("scss-syntax-node-limit"))?,
            )
            .ok_or_else(|| failure("scss-syntax-node-limit"))?;
        for (index, extend) in declarations.extends.iter().enumerate() {
            cancellation.check()?;
            let parent = extend.scope.map(scope_id).transpose()?.unwrap_or(1);
            let local_id = first_extend_id
                .checked_add(u64::try_from(index).map_err(|_| failure("scss-syntax-node-limit"))?)
                .ok_or_else(|| failure("scss-syntax-node-limit"))?;
            facts.push(SyntaxFact::new(
                local_id,
                Some(parent),
                SyntaxFactKind::Occurrence,
                source_span(file, extend.selector_start, extend.selector_end)?,
                extend.depth,
                label("scss.extend.reference"),
            ));
        }
        let mut next = first_extend_id
            .checked_add(
                u64::try_from(declarations.extends.len())
                    .map_err(|_| failure("scss-syntax-node-limit"))?,
            )
            .ok_or_else(|| failure("scss-syntax-node-limit"))?;
        for proof in &declarations.proofs {
            cancellation.check()?;
            // The certificate spans an earlier target and its later call;
            // the file root contains it, while the call scope proves the caller.
            facts.push(SyntaxFact::new(
                next,
                Some(1),
                SyntaxFactKind::Signature,
                source_span(
                    file,
                    declarations.declarations[proof.target].start,
                    declarations.calls[proof.call].name_end,
                )?,
                declarations.declarations[proof.owner].depth,
                label("scss.mixin_target.signature"),
            ));
            next = next
                .checked_add(1)
                .ok_or_else(|| failure("scss-syntax-node-limit"))?;
        }
        for gap in &declarations.parameter_gaps {
            cancellation.check()?;
            let declaration = declarations.declarations[gap.owner];
            let parent = u64::try_from(gap.owner)
                .ok()
                .and_then(|owner| owner.checked_mul(3))
                .and_then(|owner| owner.checked_add(2))
                .ok_or_else(|| failure("scss-syntax-node-limit"))?;
            facts.push(SyntaxFact::new(
                next,
                Some(parent),
                SyntaxFactKind::Signature,
                source_span(file, declaration.name_end, gap.end)?,
                declaration.depth + 1,
                label("scss.parameter_list_unavailable.signature"),
            ));
            next = next
                .checked_add(1)
                .ok_or_else(|| failure("scss-syntax-node-limit"))?;
        }
        debug_assert_eq!(facts.len(), fact_count);
        for fact in &mut facts {
            cancellation.check()?;
            let start = usize::try_from(fact.span().start_byte())
                .map_err(|_| failure("scss-source-offset-limit"))?;
            let end = usize::try_from(fact.span().end_byte())
                .map_err(|_| failure("scss-source-offset-limit"))?;
            let physical = source_span(file, view.physical(start)?, view.physical(end)?)?;
            *fact = SyntaxFact::new(
                fact.local_id(),
                fact.parent(),
                fact.kind(),
                physical,
                fact.depth(),
                fact.syntax_kind().clone(),
            );
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
        let entity_count = declarations
            .declarations
            .len()
            .checked_add(declarations.parameters.len())
            .ok_or_else(|| failure("scss-syntax-node-limit"))?;
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
                    entity_count,
                    entity_count,
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
    Keyframes,
    StyleRule,
}

impl DeclarationKind {
    fn label(self) -> &'static str {
        match self {
            Self::Variable => "scss.variable.declaration",
            Self::Mixin => "scss.mixin.declaration",
            Self::Function => "scss.function.declaration",
            Self::Keyframes => "scss.keyframes.declaration",
            Self::StyleRule => "scss.style_rule.declaration",
        }
    }

    fn signature_label(self) -> &'static str {
        match self {
            Self::StyleRule => "scss.selector.signature",
            Self::Variable | Self::Mixin | Self::Function | Self::Keyframes => {
                "scss.name.signature"
            }
        }
    }

    fn definition_label(self) -> &'static str {
        match self {
            Self::StyleRule => "scss.selector.definition",
            Self::Variable | Self::Mixin | Self::Function | Self::Keyframes => {
                "scss.identifier.definition"
            }
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
    selector_header: Option<(usize, usize)>,
    keyframes_body: bool,
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
enum ModuleLoadKind {
    Import,
    Use,
    Forward,
}

impl ModuleLoadKind {
    fn label(self) -> &'static str {
        match self {
            Self::Import => "scss.import",
            Self::Use => "scss.use",
            Self::Forward => "scss.forward",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QuotedImport {
    kind: ModuleLoadKind,
    path_start: usize,
    path_end: usize,
    scope: Option<usize>,
    depth: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SelectorExtend {
    selector_start: usize,
    selector_end: usize,
    scope: Option<usize>,
    depth: usize,
}

struct ScanResult {
    declarations: Vec<Declaration>,
    scopes: Vec<BlockScope>,
    calls: Vec<MixinCall>,
    imports: Vec<QuotedImport>,
    extends: Vec<SelectorExtend>,
    parameters: Vec<Parameter>,
    proofs: Vec<MixinProof>,
    parameter_gaps: Vec<ParameterGap>,
}

impl ScanResult {
    fn fact_count(&self) -> Result<usize, AdapterError> {
        syntax_fact_count(
            self.declarations.len(),
            self.scopes.len(),
            self.calls.len(),
            self.imports.len(),
            self.extends.len(),
        )?
        .checked_add(
            self.parameters
                .len()
                .checked_mul(3)
                .ok_or_else(|| failure("scss-syntax-node-limit"))?,
        )
        .and_then(|count| count.checked_add(self.proofs.len()))
        .and_then(|count| count.checked_add(self.parameter_gaps.len()))
        .ok_or_else(|| failure("scss-syntax-node-limit"))
    }

    fn maximum_depth(&self) -> usize {
        self.declarations
            .iter()
            .map(|declaration| declaration.depth + 1)
            .chain(self.scopes.iter().map(|scope| scope.depth))
            .chain(self.calls.iter().map(|call| call.depth))
            .chain(self.imports.iter().map(|import| import.depth))
            .chain(self.extends.iter().map(|extend| extend.depth))
            .chain(self.parameters.iter().map(|parameter| parameter.depth + 1))
            .chain(
                self.proofs
                    .iter()
                    .map(|proof| self.declarations[proof.owner].depth + 1),
            )
            .chain(
                self.parameter_gaps
                    .iter()
                    .map(|gap| self.declarations[gap.owner].depth + 1),
            )
            .max()
            .unwrap_or(0)
    }
}

fn syntax_fact_count(
    declarations: usize,
    scopes: usize,
    calls: usize,
    imports: usize,
    extends: usize,
) -> Result<usize, AdapterError> {
    declarations
        .checked_mul(3)
        .and_then(|value| value.checked_add(scopes))
        .and_then(|value| value.checked_add(calls))
        .and_then(|value| value.checked_add(imports))
        .and_then(|value| value.checked_add(extends))
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
    let mut extends = Vec::<SelectorExtend>::new();
    let mut scope_stack = Vec::<usize>::new();
    let mut interpolation_depth = 0_usize;
    let mut parentheses = 0_usize;
    let mut brackets = 0_usize;
    let mut statement_start = true;
    let mut statement_offset = None;
    for (index, &byte) in masked.iter().enumerate() {
        if index < 3 && source.starts_with(&[0xef, 0xbb, 0xbf]) {
            continue;
        }
        if index.is_multiple_of(CHECK_INTERVAL) {
            cancellation.check()?;
        }
        if interpolation_depth == 0
            && parentheses == 0
            && brackets == 0
            && statement_start
            && !byte.is_ascii_whitespace()
        {
            statement_offset = Some(index);
            if let Some(mut declaration) = parse_declaration(&masked, source, index) {
                let next = declarations
                    .len()
                    .checked_add(1)
                    .ok_or_else(|| failure("scss-syntax-node-limit"))?;
                let next_count = syntax_fact_count(
                    next,
                    scopes.len(),
                    calls.len(),
                    imports.len(),
                    extends.len(),
                )?;
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
                if syntax_fact_count(
                    declarations.len(),
                    scopes.len(),
                    next,
                    imports.len(),
                    extends.len(),
                )? > max_syntax_nodes
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
                    extends.len(),
                )?);
                if let Some(mut paths) = parse_quoted_module_paths(
                    &masked,
                    source,
                    index,
                    available,
                    scope_stack.is_empty(),
                )? {
                    for import in &mut paths {
                        import.scope = scope_stack.last().copied();
                        import.depth = scope_stack.len().saturating_mul(2).saturating_add(1);
                    }
                    imports
                        .try_reserve(paths.len())
                        .map_err(|_| SinkError::AllocationFailed)?;
                    imports.extend(paths);
                } else if scope_stack.iter().any(|&scope| {
                    scopes[scope].selector_header.is_some()
                        || scopes[scope].owner.is_some_and(|owner| {
                            declarations[owner].kind == DeclarationKind::StyleRule
                        })
                }) && let Some(mut extend) = parse_selector_extend(&masked, index)
                {
                    if syntax_fact_count(
                        declarations.len(),
                        scopes.len(),
                        calls.len(),
                        imports.len(),
                        extends.len() + 1,
                    )? > max_syntax_nodes
                    {
                        return Err(failure("scss-syntax-node-limit"));
                    }
                    extend.scope = scope_stack.last().copied();
                    extend.depth = scope_stack.len().saturating_mul(2).saturating_add(1);
                    extends
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    extends.push(extend);
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
                    extends.len(),
                )?;
                if next_count > max_syntax_nodes {
                    return Err(failure("scss-syntax-node-limit"));
                }
                let parent = scope_stack.last().copied();
                let owner = declarations
                    .last()
                    .is_some_and(|declaration| declaration.opening == Some(index))
                    .then(|| declarations.len() - 1);
                let keyframes_body = parent.is_some_and(|scope| scopes[scope].keyframes_body)
                    || statement_offset
                        .and_then(|start| keyframes_keyword_end(&masked, start))
                        .is_some();
                let selector_header = if owner.is_none()
                    && !parent.is_some_and(|scope| scopes[scope].keyframes_body)
                {
                    style_rule_header(&masked, statement_offset, index)
                } else {
                    None
                };
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
                    selector_header,
                    keyframes_body,
                    depth,
                });
                scope_stack
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                scope_stack.push(scope_index);
                statement_start = true;
                statement_offset = None;
            }
            b'}' if interpolation_depth != 0 => {
                interpolation_depth -= 1;
            }
            b'}' if parentheses == 0 && brackets == 0 => {
                if let Some(scope_index) = scope_stack.pop() {
                    let end = index + 1;
                    scopes[scope_index].end = end;
                    if let Some((name_start, name_end)) = scopes[scope_index].selector_header.take()
                    {
                        let next = declarations
                            .len()
                            .checked_add(1)
                            .ok_or_else(|| failure("scss-syntax-node-limit"))?;
                        if syntax_fact_count(
                            next,
                            scopes.len(),
                            calls.len(),
                            imports.len(),
                            extends.len(),
                        )? > max_syntax_nodes
                        {
                            return Err(failure("scss-syntax-node-limit"));
                        }
                        let declaration_index = declarations.len();
                        declarations
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        declarations.push(Declaration {
                            kind: DeclarationKind::StyleRule,
                            start: name_start,
                            end,
                            name_start,
                            name_end,
                            opening: Some(scopes[scope_index].start),
                            scope: scopes[scope_index].parent,
                            depth: scope_stack.len().saturating_mul(2).saturating_add(1),
                        });
                        scopes[scope_index].owner = Some(declaration_index);
                        scopes[scope_index].depth = scopes[scope_index].depth.saturating_add(1);
                    }
                }
                statement_start = true;
                statement_offset = None;
            }
            b';' if parentheses == 0 && brackets == 0 && interpolation_depth == 0 => {
                statement_start = true;
                statement_offset = None;
            }
            _ => {}
        }
    }
    cancellation.check()?;
    let mut result = ScanResult {
        declarations,
        scopes,
        calls,
        imports,
        extends,
        parameters: Vec::new(),
        proofs: Vec::new(),
        parameter_gaps: Vec::new(),
    };
    enrich_mixins(source, &masked, &mut result, max_syntax_nodes, cancellation)?;
    Ok(result)
}

fn style_rule_header(
    source: &[u8],
    start: Option<usize>,
    opening: usize,
) -> Option<(usize, usize)> {
    let start = start?;
    let header = source.get(start..opening)?;
    let first = *header.first()?;
    if matches!(first, b'@' | b'$') {
        return None;
    }
    let last = header
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())?;
    if header.get(last) == Some(&b':') {
        return None;
    }
    Some((start, start.checked_add(last)?.checked_add(1)?))
}

fn keyframes_keyword_end(source: &[u8], start: usize) -> Option<usize> {
    let tail = source.get(start..)?;
    let length = tail.iter().position(u8::is_ascii_whitespace)?;
    let keyword = tail.get(..length)?;
    if keyword != b"@keyframes" {
        let vendor = keyword.strip_prefix(b"@-")?.strip_suffix(b"-keyframes")?;
        if vendor.is_empty()
            || !vendor
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            return None;
        }
    }
    start.checked_add(length)
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
            kind: ModuleLoadKind::Import,
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

fn parse_quoted_module_paths(
    masked: &[u8],
    source: &[u8],
    start: usize,
    maximum_paths: usize,
    top_level: bool,
) -> Result<Option<Vec<QuotedImport>>, AdapterError> {
    if let Some(imports) = parse_quoted_import_paths(source, start, maximum_paths)? {
        return Ok(Some(imports));
    }
    if !top_level {
        return Ok(None);
    }
    let tail = masked.get(start..).unwrap_or_default();
    let (kind, keyword) = if tail.starts_with(b"@use") {
        (ModuleLoadKind::Use, b"@use".as_slice())
    } else if tail.starts_with(b"@forward") {
        (ModuleLoadKind::Forward, b"@forward".as_slice())
    } else {
        return Ok(None);
    };
    let Some(after_keyword) = start.checked_add(keyword.len()) else {
        return Ok(None);
    };
    if !masked
        .get(after_keyword)
        .is_some_and(u8::is_ascii_whitespace)
    {
        return Ok(None);
    }
    let Some(quote_start) = skip_module_gap(source, after_keyword) else {
        return Ok(None);
    };
    let Some(&quote) = source.get(quote_start) else {
        return Ok(None);
    };
    if !matches!(quote, b'\'' | b'"') {
        return Ok(None);
    }
    let path_start = quote_start + 1;
    let mut path_end = path_start;
    while let Some(&byte) = source.get(path_end) {
        if byte == b'\\' {
            path_end = path_end.saturating_add(2);
        } else if byte == quote {
            break;
        } else {
            path_end += 1;
        }
    }
    let Some(path) = source.get(path_start..path_end) else {
        return Ok(None);
    };
    if path.is_empty() || path.windows(2).any(|pair| pair == b"#{") {
        return Ok(None);
    }
    if source.get(path_end) != Some(&quote) {
        return Ok(None);
    }
    let mut cursor = skip_whitespace(masked, path_end + 1);
    if let Some(after_as) = module_clause(masked, cursor, b"as") {
        cursor = skip_whitespace(masked, after_as);
        cursor = match kind {
            ModuleLoadKind::Use if masked.get(cursor) == Some(&b'*') => cursor + 1,
            ModuleLoadKind::Use => match identifier_end(masked, cursor) {
                Some(end) => end,
                None => return Ok(None),
            },
            ModuleLoadKind::Forward => {
                let Some(end) = identifier_end(masked, cursor) else {
                    return Ok(None);
                };
                if end <= cursor + 1
                    || masked.get(end - 1) != Some(&b'-')
                    || masked.get(end) != Some(&b'*')
                {
                    return Ok(None);
                }
                end + 1
            }
            ModuleLoadKind::Import => return Ok(None),
        };
        cursor = skip_whitespace(masked, cursor);
    }
    if kind == ModuleLoadKind::Forward {
        let after_visibility = module_clause(masked, cursor, b"show")
            .or_else(|| module_clause(masked, cursor, b"hide"));
        if let Some(after_visibility) = after_visibility {
            cursor = skip_whitespace(masked, after_visibility);
            loop {
                if masked.get(cursor) == Some(&b'$') {
                    cursor += 1;
                }
                let Some(end) = identifier_end(masked, cursor) else {
                    return Ok(None);
                };
                cursor = skip_whitespace(masked, end);
                if masked.get(cursor) != Some(&b',') {
                    break;
                }
                cursor = skip_whitespace(masked, cursor + 1);
            }
        }
    }
    if let Some(after_with) = module_clause(masked, cursor, b"with") {
        cursor = skip_whitespace(masked, after_with);
        let Some(end) = balanced_end(masked, cursor, b'(', b')') else {
            return Ok(None);
        };
        cursor = skip_whitespace(masked, end);
    }
    if masked.get(cursor) != Some(&b';') {
        return Ok(None);
    }
    if maximum_paths == 0 {
        return Err(failure("scss-syntax-node-limit"));
    }
    Ok(Some(vec![QuotedImport {
        kind,
        path_start,
        path_end,
        scope: None,
        depth: 0,
    }]))
}

fn module_clause(source: &[u8], start: usize, keyword: &[u8]) -> Option<usize> {
    let end = start.checked_add(keyword.len())?;
    (source.get(start..end) == Some(keyword)
        && source.get(end).is_some_and(u8::is_ascii_whitespace))
    .then_some(end)
}

fn skip_module_gap(source: &[u8], mut cursor: usize) -> Option<usize> {
    loop {
        cursor = skip_whitespace(source, cursor);
        let tail = source.get(cursor..)?;
        if tail.starts_with(b"/*") {
            let end = tail.get(2..)?.windows(2).position(|pair| pair == b"*/")?;
            cursor = cursor.checked_add(end)?.checked_add(4)?;
        } else if tail.starts_with(b"//") {
            let end = tail.iter().position(|byte| *byte == b'\n')?;
            cursor = cursor.checked_add(end)?.checked_add(1)?;
        } else {
            return Some(cursor);
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

fn parse_selector_extend(source: &[u8], start: usize) -> Option<SelectorExtend> {
    let after_keyword = start.checked_add(b"@extend".len())?;
    if !source.get(start..)?.starts_with(b"@extend")
        || !source.get(after_keyword)?.is_ascii_whitespace()
    {
        return None;
    }
    let selector_start = skip_whitespace(source, after_keyword);
    if !matches!(
        source.get(selector_start).copied(),
        Some(b'.' | b'%' | b'#')
    ) {
        return None;
    }
    let selector_end = identifier_end(source, selector_start.checked_add(1)?)?;
    let mut cursor = skip_whitespace(source, selector_end);
    if source.get(cursor..)?.starts_with(b"!optional") {
        cursor = skip_whitespace(source, cursor.checked_add(b"!optional".len())?);
    }
    (source.get(cursor) == Some(&b';')).then_some(SelectorExtend {
        selector_start,
        selector_end,
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
            let end = keyframes_keyword_end(masked, start)?;
            (DeclarationKind::Keyframes, end.checked_sub(start)?)
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
        DeclarationKind::Mixin | DeclarationKind::Function | DeclarationKind::Keyframes => {
            let mut opening = after_name;
            if kind != DeclarationKind::Keyframes && masked.get(opening) == Some(&b'(') {
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
        DeclarationKind::StyleRule => return None,
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

// Source-owned SCSS enrichment uses the existing scanner's balanced declarations.
// Certificates cover only earlier, unique file mixins and direct empty includes;
// compiler evaluation and imported or conditional namespaces remain unavailable.

#[derive(Clone, Copy, Debug)]
struct Parameter {
    owner: usize,
    start: usize,
    end: usize,
    name_start: usize,
    name_end: usize,
    depth: usize,
}

#[derive(Clone, Copy, Debug)]
struct MixinProof {
    owner: usize,
    target: usize,
    call: usize,
}

#[derive(Clone, Copy, Debug)]
struct ParameterGap {
    owner: usize,
    end: usize,
}

struct MixinWork<'a> {
    remaining: usize,
    cancellation: &'a Cancellation,
}

impl MixinWork<'_> {
    fn visit(&mut self) -> Result<(), AdapterError> {
        self.cancellation.check()?;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| failure("scss-mixin-proof-work-limit"))?;
        Ok(())
    }

    fn whitespace(&mut self, source: &[u8], mut cursor: usize) -> Result<usize, AdapterError> {
        while source.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            self.visit()?;
            cursor += 1;
        }
        Ok(cursor)
    }

    fn name_end(&mut self, source: &[u8], start: usize) -> Result<Option<usize>, AdapterError> {
        self.visit()?;
        if !source
            .get(start)
            .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'-'))
        {
            return Ok(None);
        }
        if source.get(start) == Some(&b'-')
            && !source
                .get(start + 1)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            return Ok(None);
        }
        let mut end = start + 1;
        while source
            .get(end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            self.visit()?;
            end += 1;
        }
        Ok(Some(end))
    }
}

fn same_mixin_name(
    left: &[u8],
    right: &[u8],
    work: &mut MixinWork<'_>,
) -> Result<bool, AdapterError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (&left, &right) in left.iter().zip(right) {
        work.visit()?;
        if left != right && !(matches!(left, b'-' | b'_') && matches!(right, b'-' | b'_')) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn complete_lexical_input(source: &[u8], work: &mut MixinWork<'_>) -> Result<bool, AdapterError> {
    let mut cursor = 0;
    while cursor < source.len() {
        work.visit()?;
        let line = source.get(cursor..cursor + 2) == Some(b"//");
        let block = source.get(cursor..cursor + 2) == Some(b"/*");
        if line || block {
            cursor += 2;
            let mut closed = line;
            while cursor < source.len() {
                work.visit()?;
                if line && source[cursor] == b'\n' {
                    break;
                }
                if block && source.get(cursor..cursor + 2) == Some(b"*/") {
                    cursor += 2;
                    closed = true;
                    break;
                }
                cursor += 1;
            }
            if !closed {
                return Ok(false);
            }
        } else if matches!(source[cursor], b'\'' | b'"') {
            let quote = source[cursor];
            cursor += 1;
            let mut closed = false;
            while cursor < source.len() {
                work.visit()?;
                if source[cursor] == b'\\' {
                    cursor = cursor.saturating_add(2);
                } else {
                    let byte = source[cursor];
                    cursor += 1;
                    if byte == quote {
                        closed = true;
                        break;
                    }
                }
            }
            if !closed || cursor > source.len() {
                return Ok(false);
            }
        } else {
            cursor += 1;
        }
    }
    Ok(true)
}

fn complete_mixin_environment(
    source: &[u8],
    masked: &[u8],
    declarations: &[Declaration],
    work: &mut MixinWork<'_>,
) -> Result<bool, AdapterError> {
    if !complete_lexical_input(source, work)? {
        return Ok(false);
    }
    let mut braces = 0_usize;
    let mut parentheses = 0_usize;
    let mut brackets = 0_usize;
    let mut delimiters = [0_u8; 128];
    let mut depth = 0_usize;
    for (index, &byte) in masked.iter().enumerate() {
        work.visit()?;
        if matches!(byte, b'{' | b'(' | b'[') {
            let Some(slot) = delimiters.get_mut(depth) else {
                return Ok(false);
            };
            *slot = byte;
            depth += 1;
        } else if matches!(byte, b'}' | b')' | b']') {
            let Some(parent) = depth.checked_sub(1) else {
                return Ok(false);
            };
            let expected = match byte {
                b'}' => b'{',
                b')' => b'(',
                _ => b'[',
            };
            if delimiters[parent] != expected {
                return Ok(false);
            }
            depth = parent;
        }
        match byte {
            b'{' => {
                if masked.get(index.wrapping_sub(1)) == Some(&b'#') {
                    return Ok(false);
                }
                braces += 1;
            }
            b'}' => {
                let Some(next) = braces.checked_sub(1) else {
                    return Ok(false);
                };
                braces = next;
            }
            b'(' => parentheses += 1,
            b')' => {
                let Some(next) = parentheses.checked_sub(1) else {
                    return Ok(false);
                };
                parentheses = next;
            }
            b'[' => brackets += 1,
            b']' => {
                let Some(next) = brackets.checked_sub(1) else {
                    return Ok(false);
                };
                brackets = next;
            }
            b'@' => {
                let Some(end) = work.name_end(masked, index + 1)? else {
                    return Ok(false);
                };
                let keyword = &masked[index + 1..end];
                match keyword {
                    b"mixin" => {
                        let mut matches = 0;
                        for declaration in declarations {
                            work.visit()?;
                            if declaration.start == index
                                && declaration.kind == DeclarationKind::Mixin
                                && declaration.scope.is_none()
                            {
                                matches += 1;
                            }
                        }
                        if matches != 1 {
                            return Ok(false);
                        }
                    }
                    b"function" | b"include" | b"extend" | b"if" | b"else" | b"return"
                    | b"media" | b"supports" | b"keyframes" | b"-webkit-keyframes" | b"debug"
                    | b"warn" | b"error" | b"for" | b"each" | b"while" => {}
                    _ => return Ok(false),
                }
            }
            _ => {}
        }
    }
    Ok(braces == 0 && parentheses == 0 && brackets == 0)
}

fn simple_parameters(
    source: &[u8],
    masked: &[u8],
    owner: usize,
    declaration: Declaration,
    maximum_parameters: usize,
    work: &mut MixinWork<'_>,
) -> Result<Option<Vec<Parameter>>, AdapterError> {
    let Some(opening) = declaration.opening else {
        return Ok(None);
    };
    let mut cursor = work.whitespace(masked, declaration.name_end)?;
    if cursor == opening {
        return Ok(Some(Vec::new()));
    }
    if masked.get(cursor) != Some(&b'(') {
        return Ok(None);
    }
    cursor += 1;
    let mut parameters: Vec<Parameter> = Vec::new();
    loop {
        work.visit()?;
        cursor = work.whitespace(masked, cursor)?;
        if masked.get(cursor) == Some(&b')') {
            return Ok((work.whitespace(masked, cursor + 1)? == opening).then_some(parameters));
        }
        let start = cursor;
        if masked.get(cursor) != Some(&b'$') {
            return Ok(None);
        }
        let name_start = cursor + 1;
        let Some(name_end) = work.name_end(masked, name_start)? else {
            return Ok(None);
        };
        if source[name_start..name_end] != masked[name_start..name_end] {
            return Ok(None);
        }
        for previous in &parameters {
            work.visit()?;
            if same_mixin_name(
                &source[previous.name_start..previous.name_end],
                &source[name_start..name_end],
                work,
            )? {
                return Ok(None);
            }
        }
        cursor = work.whitespace(masked, name_end)?;
        if masked.get(cursor) == Some(&b':') {
            cursor += 1;
            let value_start = work.whitespace(masked, cursor)?;
            cursor = value_start;
            while cursor < opening && !matches!(masked[cursor], b',' | b')') {
                work.visit()?;
                cursor += 1;
            }
            let value = source.get(value_start..cursor).unwrap_or_default();
            let value = value.trim_ascii();
            let mut number = value.first().is_some_and(u8::is_ascii_digit)
                && value.last().is_some_and(u8::is_ascii_digit);
            let mut dots = 0;
            for &byte in value {
                work.visit()?;
                dots += usize::from(byte == b'.');
                number &= byte.is_ascii_digit() || byte == b'.';
            }
            if !(matches!(value, b"true" | b"false" | b"null") || number && dots <= 1) {
                return Ok(None);
            }
        }
        let end = cursor;
        if parameters.len() >= maximum_parameters {
            return Err(failure("scss-syntax-node-limit"));
        }
        parameters
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        parameters.push(Parameter {
            owner,
            start,
            end,
            name_start,
            name_end,
            depth: declaration.depth + 1,
        });
        match masked.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b')') => {}
            _ => return Ok(None),
        }
    }
}

fn empty_include(
    masked: &[u8],
    call: MixinCall,
    work: &mut MixinWork<'_>,
) -> Result<bool, AdapterError> {
    let mut cursor = work.whitespace(masked, call.name_end)?;
    if masked.get(cursor) == Some(&b'(') {
        cursor += 1;
        while masked.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            work.visit()?;
            cursor += 1;
        }
        if masked.get(cursor) != Some(&b')') {
            return Ok(false);
        }
        cursor = work.whitespace(masked, cursor + 1)?;
    }
    Ok(masked.get(cursor) == Some(&b';'))
}

fn enrich_mixins(
    source: &[u8],
    masked: &[u8],
    result: &mut ScanResult,
    max_syntax_nodes: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut work = MixinWork {
        remaining: max_syntax_nodes
            .min(2_097_152)
            .checked_mul(128)
            .ok_or_else(|| failure("scss-mixin-proof-work-limit"))?,
        cancellation,
    };
    let lexical = complete_lexical_input(source, &mut work)?;
    for (owner, &declaration) in result.declarations.iter().enumerate() {
        work.visit()?;
        if !matches!(
            declaration.kind,
            DeclarationKind::Mixin | DeclarationKind::Function
        ) {
            continue;
        }
        let available_parameters = max_syntax_nodes.saturating_sub(result.fact_count()?) / 3;
        match lexical
            .then(|| {
                simple_parameters(
                    source,
                    masked,
                    owner,
                    declaration,
                    available_parameters,
                    &mut work,
                )
            })
            .transpose()?
            .flatten()
        {
            Some(parameters) => {
                result
                    .parameters
                    .try_reserve(parameters.len())
                    .map_err(|_| SinkError::AllocationFailed)?;
                result.parameters.extend(parameters);
            }
            None => {
                result
                    .parameter_gaps
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                result.parameter_gaps.push(ParameterGap {
                    owner,
                    end: declaration.opening.unwrap_or(declaration.end),
                });
            }
        }
        if result.fact_count()? > max_syntax_nodes {
            return Err(failure("scss-syntax-node-limit"));
        }
    }
    if !complete_mixin_environment(source, masked, &result.declarations, &mut work)? {
        return Ok(());
    }
    for (call_index, &call) in result.calls.iter().enumerate() {
        work.visit()?;
        if work.name_end(masked, call.name_start)? != Some(call.name_end)
            || !empty_include(masked, call, &mut work)?
        {
            continue;
        }
        let Some(scope) = call.scope.and_then(|index| result.scopes.get(index)) else {
            continue;
        };
        let Some(owner) = scope.owner else { continue };
        let caller = result.declarations[owner];
        if scope.parent.is_some()
            || caller.scope.is_some()
            || caller.kind != DeclarationKind::Mixin
            || caller.opening != Some(scope.start)
            || caller.end != scope.end
        {
            continue;
        }
        let mut target = None;
        let mut candidates = 0;
        let mut owners = 0;
        for (index, declaration) in result.declarations.iter().enumerate() {
            work.visit()?;
            if declaration.kind != DeclarationKind::Mixin {
                continue;
            }
            if same_mixin_name(
                &source[declaration.name_start..declaration.name_end],
                &source[caller.name_start..caller.name_end],
                &mut work,
            )? {
                owners += 1;
            }
            if same_mixin_name(
                &source[declaration.name_start..declaration.name_end],
                &source[call.name_start..call.name_end],
                &mut work,
            )? {
                candidates += 1;
                target = Some(index);
            }
        }
        let Some(target) = target.filter(|_| candidates == 1 && owners == 1) else {
            continue;
        };
        let target_declaration = result.declarations[target];
        let mut unsupported_header = false;
        for parameter in &result.parameters {
            work.visit()?;
            unsupported_header |= parameter.owner == target;
        }
        for gap in &result.parameter_gaps {
            work.visit()?;
            unsupported_header |= gap.owner == target || gap.owner == owner;
        }
        if target_declaration.scope.is_some()
            || target_declaration.end > caller.start
            || unsupported_header
        {
            continue;
        }
        result
            .proofs
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        result.proofs.push(MixinProof {
            owner,
            target,
            call: call_index,
        });
        if result.fact_count()? > max_syntax_nodes {
            return Err(failure("scss-syntax-node-limit"));
        }
    }
    cancellation.check()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_nested_declarations_without_comment_or_string_lookalikes() {
        let source = b"$accent: #123;\n@mixin card($r) { border-radius: $r; }\n@function scale($x) { @return $x * 2; }\n/* $ghost: red; */\n.note { content: \"@mixin phantom {}\"; $nested: blue; }\n";
        let result = scan_source(source, 64, &Cancellation::new()).unwrap();
        assert_eq!(result.declarations.len(), 5);
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
                (&b".note"[..], DeclarationKind::StyleRule),
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
    fn captures_simple_selector_extends_inside_style_rules_only() {
        let source = b".error { color: red; }\n%base { color: blue; }\n.serious { @extend .error; @extend %base !optional; content: \"@extend .ghost;\"; /* @extend #phantom; */ }\n@extend .outside;\n";
        let result = scan_source(source, 128, &Cancellation::new()).unwrap();
        assert_eq!(result.extends.len(), 2);
        assert_eq!(
            result
                .extends
                .iter()
                .map(|extend| &source[extend.selector_start..extend.selector_end])
                .collect::<Vec<_>>(),
            [&b".error"[..], &b"%base"[..]]
        );
        assert_eq!(result.extends[0].scope, result.extends[1].scope);
        for source in [
            &b".rule { @extend .missing }"[..],
            &b".rule { @extend .one.two; }"[..],
            &b".rule { @extend .one:hover; }"[..],
            &b".rule { @extend #{dynamic}; }"[..],
        ] {
            assert!(
                scan_source(source, 128, &Cancellation::new())
                    .unwrap()
                    .extends
                    .is_empty()
            );
        }
        assert!(scan_source(b".rule { @extend .one; }", 5, &Cancellation::new()).is_err());
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

    #[test]
    fn captures_static_use_and_forward_urls_with_directive_identity() {
        let source = b"@use \"theme/colors\" as colors;\n@use 'mixins' as *;\n@use \"scale\" with ($base: 1);\n@forward \"tokens\";\n@forward 'functions' as util-* show $base, run;\n@forward \"private\" hide $secret with ($base: 2);\n@use /* module note */ \"commented\";\n.note { content: \"@use 'ghost';\"; @use \"nested\"; }\n// @forward 'comment';\n";
        let result = scan_source(source, 64, &Cancellation::new()).unwrap();
        assert_eq!(result.imports.len(), 7);
        assert_eq!(
            result
                .imports
                .iter()
                .map(|import| (&source[import.path_start..import.path_end], import.kind))
                .collect::<Vec<_>>(),
            [
                (&b"theme/colors"[..], ModuleLoadKind::Use),
                (&b"mixins"[..], ModuleLoadKind::Use),
                (&b"scale"[..], ModuleLoadKind::Use),
                (&b"tokens"[..], ModuleLoadKind::Forward),
                (&b"functions"[..], ModuleLoadKind::Forward),
                (&b"private"[..], ModuleLoadKind::Forward),
                (&b"commented"[..], ModuleLoadKind::Use),
            ]
        );
        for source in [
            &b"@use \"unfinished\""[..],
            &b"@use \"theme\", \"other\";"[..],
            &b"@use \"theme\" as;"[..],
            &b"@forward \"theme\" hide;"[..],
            &b"@forward \"theme\" as util;"[..],
            &b"@use \"theme/#{$dynamic}\";"[..],
        ] {
            assert!(
                scan_source(source, 64, &Cancellation::new())
                    .unwrap()
                    .imports
                    .is_empty()
            );
        }
        assert!(scan_source(b"@use \"module\";", 1, &Cancellation::new()).is_err());
    }

    #[test]
    fn completes_nested_style_rules_without_promoting_at_rules_or_property_groups() {
        let source = b"@media screen { .card, .tile { &:hover { color: red; } font: { size: 10px; } } }\n@mixin helper { .inside { color: blue; } }\n.unclosed {";
        let result = scan_source(source, 64, &Cancellation::new()).unwrap();
        let mut selectors = result
            .declarations
            .iter()
            .filter(|declaration| declaration.kind == DeclarationKind::StyleRule)
            .map(|declaration| &source[declaration.name_start..declaration.name_end])
            .collect::<Vec<_>>();
        selectors.sort_unstable();
        assert_eq!(
            selectors,
            [&b"&:hover"[..], &b".card, .tile"[..], &b".inside"[..]]
        );
        assert!(
            scan_source(b".unclosed {", 64, &Cancellation::new())
                .unwrap()
                .declarations
                .is_empty()
        );
        assert!(scan_source(b".complete {}", 4, &Cancellation::new()).is_err());
    }

    #[test]
    fn separates_keyframe_names_from_timeline_steps_and_style_selectors() {
        let source = b"@keyframes pulse { from { opacity: 0; } @if $enabled { 50% { opacity: .5; } } }\n@-webkit-keyframes fade { to { opacity: 1; } }\n@keyframes #{$dynamic} { 25% { opacity: 1; } }\n.actual { color: red; }";
        let result = scan_source(source, 128, &Cancellation::new()).unwrap();
        let names = |kind| {
            result
                .declarations
                .iter()
                .filter(|declaration| declaration.kind == kind)
                .map(|declaration| &source[declaration.name_start..declaration.name_end])
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(DeclarationKind::Keyframes),
            [&b"pulse"[..], &b"fade"[..]]
        );
        assert_eq!(names(DeclarationKind::StyleRule), [&b".actual"[..]]);
    }
}

#[cfg(test)]
mod mixin_proof_tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn original_formal_owner_and_two_call_witnesses_are_source_bound() {
        let raw = include_bytes!(
            "../../rootlight-service/src/tests/fixtures/scss/original-rounded-corners-mixin.scss"
        );
        let scan = scan_source(raw, 16_384, &Cancellation::new()).unwrap();
        let formals: Vec<_> = scan
            .parameters
            .iter()
            .filter(|parameter| parameter.name_start == 1280)
            .collect();
        assert_eq!(formals.len(), 1);
        assert_eq!(&raw[formals[0].name_start..formals[0].name_end], b"isDark");
        let owner = scan.declarations[formals[0].owner];
        assert_eq!(&raw[owner.name_start..owner.name_end], b"rounded-corners");
        assert_eq!(scan.proofs.len(), 2);
        for proof in &scan.proofs {
            let call = scan.calls[proof.call];
            assert!(matches!(call.name_start, 1402 | 1768));
            let target = scan.declarations[proof.target];
            assert_eq!((target.name_start, target.name_end), (945, 964));
            assert!(target.end < scan.declarations[proof.owner].start);
        }
        assert!(scan.fact_count().unwrap() > 1);
        assert!(scan.parameter_gaps.is_empty());
    }

    #[test]
    fn comparison_work_stops_inside_equal_name_prefixes() {
        let cancellation = Cancellation::new();
        let mut work = MixinWork {
            remaining: 3,
            cancellation: &cancellation,
        };
        assert!(
            matches!(same_mixin_name(b"long_mixin", b"long-mixin", &mut work),
            Err(AdapterError::ProviderFailed { code }) if code.as_str() == "scss-mixin-proof-work-limit")
        );
    }

    #[test]
    fn proof_work_observes_cancellation_after_prior_successful_work() {
        let cancellation = Cancellation::new();
        let mut work = MixinWork {
            remaining: 32,
            cancellation: &cancellation,
        };
        assert!(same_mixin_name(b"same", b"same", &mut work).unwrap());
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            work.visit(),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
