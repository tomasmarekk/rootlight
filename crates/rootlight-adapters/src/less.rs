//! Guarded native Less syntax and a closed, source-local mixin declaration proof.
//! Expansion, imports, overload selection and dynamic scope remain explicit gaps.

mod control;

use crate::source_projection;

use std::collections::HashMap;

use raffia::{
    ParserBuilder, Syntax,
    ast::*,
    pos::{Span, Spanned},
};
use rootlight_adapter_sdk::{
    AdapterError, CoverageReport, DescriptorError, DiagnosticCode, DomainCoverage, EncodingId,
    LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider, ParseReport, ParseRequest,
    ResourceUsage, SinkError, StreamEnd, SyntaxFact, SyntaxFactBatch, SyntaxFactKind,
    SyntaxFactSink, SyntaxKindLabel,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId, content_hash};
use rootlight_ir::{AnalysisTier, CoverageStatus, FactDomain, SourceSpan};

/// A native Less frontend with independently bounded parser and projection work.
pub struct LessParseProvider {
    capabilities: ParseCapabilities,
}

impl LessParseProvider {
    /// Identifies the bridge; retained source manifests also bind Raffia inputs.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("less.rs"))
    }

    /// Creates a bounded UTF-8 and BOM-backed UTF-16 frontend.
    ///
    /// # Errors
    /// Returns a descriptor error for invalid ceilings.
    /// # Panics
    /// Panics only if the fixed language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        Ok(Self {
            capabilities: ParseCapabilities::new(
                vec![LanguageId::new("less").expect("fixed Less language")],
                vec![
                    EncodingId::utf8(),
                    EncodingId::new("utf-16le").expect("fixed UTF-16LE label"),
                    EncodingId::new("utf-16be").expect("fixed UTF-16BE label"),
                ],
                max_source_bytes,
                max_syntax_nodes.min(2_097_152),
                128,
                0,
                false,
                true,
                true,
                1,
                MemoryEnforcement::Unavailable,
            )?,
        })
    }

    /// Stages the same required facts under the same parser and source guards.
    /// # Errors
    /// Returns source, quota, allocation or cancellation failures without recovery.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancel: &Cancellation,
    ) -> Result<usize, AdapterError> {
        Ok(self.project(request, cancel)?.facts.len())
    }

    fn project(
        &self,
        request: &ParseRequest<'_>,
        cancel: &Cancellation,
    ) -> Result<Output, AdapterError> {
        cancel.check()?;
        if request.language().as_str() != "less"
            || !matches!(
                request.encoding().as_str(),
                "utf-8" | "utf-16le" | "utf-16be"
            )
            || !request.included_ranges().is_empty()
        {
            return Err(failure("less-source-context-unsupported"));
        }
        let raw = request.source().bytes();
        if raw.len() > self.capabilities.max_source_bytes() {
            return Err(failure("less-source-byte-limit"));
        }
        let view = source_projection::View::new(
            raw,
            request.encoding().as_str(),
            self.capabilities
                .max_source_bytes()
                .min(request.limits().max_source_bytes()),
            request.limits().max_reported_memory_bytes(),
            cancel,
        )?;
        let source = view.text();
        if source.contains('\0') {
            return Err(failure("less-invalid-source-nul"));
        }
        let text = source.strip_prefix('\u{feff}').unwrap_or(source);
        let maximum = request
            .limits()
            .max_syntax_nodes()
            .min(self.capabilities.max_syntax_nodes());
        let max_depth = request
            .limits()
            .max_syntax_depth()
            .min(self.capabilities.max_syntax_depth());
        let control = control::Control::new(maximum, max_depth, cancel)?;
        let mut parser = ParserBuilder::new(text)
            .syntax(Syntax::Less)
            .control(control.hook())
            .build();
        let parsed = parser.parse::<Stylesheet>();
        control.check()?;
        for error in parser.recoverable_errors() {
            cancel.check()?;
            if matches!(error.kind, raffia::error::ErrorKind::ControlledStop) {
                return Err(failure("less-parser-controlled-stop"));
            }
        }
        let (tree, recovery) = match parsed {
            Ok(tree) => match parser.recoverable_errors().first() {
                Some(error) => (None, Some(error.span.clone())),
                None => (Some(tree), None),
            },
            Err(error) => {
                if matches!(error.kind, raffia::error::ErrorKind::ControlledStop) {
                    return Err(failure("less-parser-controlled-stop"));
                }
                (None, Some(error.span))
            }
        };
        let mut result = Projection {
            facts: Vec::new(),
            source,
            base: source.len() - text.len(),
            file: request.source().source_ref().span().file(),
            maximum,
            max_depth,
            depth: 0,
            partial: recovery.is_some(),
            coordinates: &view,
        };
        let root = result.push(
            None,
            SyntaxFactKind::Root,
            0,
            source.len(),
            0,
            "less.source_file",
        )?;
        result.push(
            Some(root),
            SyntaxFactKind::Signature,
            0,
            source.len(),
            1,
            if result.partial {
                "less.partial_parse.signature"
            } else {
                "less.clean_parse.signature"
            },
        )?;
        if let Some(span) = recovery {
            result.native(
                root,
                SyntaxFactKind::ErrorRecovery,
                &span,
                1,
                "less.rejected_syntax",
            )?;
        } else if let Some(tree) = tree {
            let census = Census::new(&tree, text, cancel)?;
            if census.closed {
                result.push(
                    Some(root),
                    SyntaxFactKind::Signature,
                    0,
                    source.len(),
                    1,
                    "less.closed_mixin_namespace.signature",
                )?;
            }
            for statement in &tree.statements {
                cancel.check()?;
                match statement {
                    Statement::LessMixinDefinition(mixin) => {
                        result.mixin(mixin, root, &census, cancel)?
                    }
                    _ => result.unsupported(root, statement.span(), 1)?,
                }
            }
        }
        cancel.check()?;
        Ok(Output {
            facts: result.facts,
            depth: result.depth,
            partial: result.partial,
        })
    }
}

impl ParseProvider for LessParseProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }
    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancel: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let projected = self.project(request, cancel)?;
        let batch_records = request
            .limits()
            .syntax_stream()
            .batch()
            .max_records()
            .min(32);
        for batch in projected.facts.chunks(batch_records) {
            cancel.check()?;
            let mut facts = Vec::new();
            facts
                .try_reserve_exact(batch.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            facts.extend_from_slice(batch);
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), facts, Vec::new()),
                cancel,
            )?;
        }
        let mut domains = Vec::new();
        domains
            .try_reserve_exact(4)
            .map_err(|_| SinkError::AllocationFailed)?;
        domains.push(DomainCoverage::new(
            FactDomain::Files,
            CoverageStatus::Complete,
            1,
            1,
            0,
        )?);
        for domain in [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ] {
            domains.push(DomainCoverage::new(
                domain,
                if projected.partial {
                    CoverageStatus::Unknown
                } else {
                    CoverageStatus::Bounded
                },
                0,
                0,
                0,
            )?);
        }
        let bytes = request.source().bytes().len();
        let usage = sink.staged_usage();
        Ok(ParseReport::new(
            CoverageReport::new(
                AnalysisTier::TierD,
                if projected.partial {
                    CoverageStatus::Unknown
                } else {
                    CoverageStatus::Bounded
                },
                bytes,
                bytes,
                0,
                domains,
            )?,
            ResourceUsage::new(
                bytes,
                projected.facts.len(),
                projected.facts.len(),
                projected.depth,
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

fn simple_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '_' | '-'))
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn mixin_name<'a>(name: &'a LessMixinName<'_>) -> Option<&'a Span> {
    let (ident, span) = match name {
        LessMixinName::ClassSelector(value) => (&value.name, &value.span),
        LessMixinName::IdSelector(value) => (&value.name, &value.span),
    };
    matches!(ident, InterpolableIdent::Literal(value) if simple_name(value.raw)).then_some(span)
}

fn parameter_name<'a, 's>(parameter: &'a LessMixinParameter<'s>) -> Option<&'a Ident<'s>> {
    match parameter {
        LessMixinParameter::Named(value) => match &value.name {
            LessMixinParameterName::Variable(variable) if simple_name(variable.name.raw) => {
                Some(&variable.name)
            }
            _ => None,
        },
        _ => None,
    }
}

fn plain_value(value: &ComponentValue<'_>) -> bool {
    matches!(value, ComponentValue::LessVariable(variable) if simple_name(variable.name.raw))
        || matches!(
            value,
            ComponentValue::Number(_)
                | ComponentValue::Dimension(_)
                | ComponentValue::Percentage(_)
                | ComponentValue::HexColor(_)
                | ComponentValue::InterpolableIdent(InterpolableIdent::Literal(_))
                | ComponentValue::InterpolableStr(InterpolableStr::Literal(_))
                | ComponentValue::Delimiter(_)
        )
}

struct Census<'a> {
    names: HashMap<&'a str, Option<usize>>,
    closed: bool,
}

impl<'a> Census<'a> {
    fn new(
        tree: &Stylesheet<'_>,
        text: &'a str,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            names: HashMap::new(),
            closed: true,
        };
        result
            .names
            .try_reserve(tree.statements.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for statement in &tree.statements {
            cancel.check()?;
            let Statement::LessMixinDefinition(mixin) = statement else {
                result.closed = false;
                continue;
            };
            let name = mixin_name(&mixin.name).and_then(|span| text.get(span.start..span.end));
            let Some(name) = name else {
                result.closed = false;
                continue;
            };
            let mut parameters = std::collections::HashSet::new();
            parameters
                .try_reserve(mixin.params.params.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            let mut eligible = mixin.guard.is_none();
            for parameter in &mixin.params.params {
                cancel.check()?;
                let Some(name) = parameter_name(parameter) else {
                    eligible = false;
                    continue;
                };
                eligible &= matches!(parameter, LessMixinParameter::Named(value) if value.value.is_none())
                    && parameters.insert(name.raw);
            }
            if !eligible {
                result.closed = false;
            }
            result
                .names
                .entry(name)
                .and_modify(|value| *value = None)
                .or_insert(eligible.then_some(mixin.params.params.len()));
        }
        for statement in &tree.statements {
            cancel.check()?;
            if let Statement::LessMixinDefinition(mixin) = statement {
                for statement in &mixin.block.statements {
                    cancel.check()?;
                    let safe = match statement {
                        Statement::Declaration(value) => {
                            let mut safe = true;
                            for value in &value.value {
                                cancel.check()?;
                                safe &= plain_value(value);
                            }
                            safe
                        }
                        Statement::LessMixinCall(call) => {
                            result.target(call, text, cancel)?.is_some()
                        }
                        _ => false,
                    };
                    if !safe {
                        result.closed = false;
                    }
                }
            }
        }
        Ok(result)
    }

    fn target(
        &self,
        call: &LessMixinCall<'_>,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<usize>, AdapterError> {
        cancel.check()?;
        let [callee] = call.callee.children.as_slice() else {
            return Ok(None);
        };
        if callee.combinator.is_some() || call.important.is_some() {
            return Ok(None);
        }
        let Some(span) = mixin_name(&callee.name) else {
            return Ok(None);
        };
        let Some(name) = text.get(span.start..span.end) else {
            return Ok(None);
        };
        let Some(arguments) = call.args.as_ref() else {
            return Ok(None);
        };
        for argument in &arguments.args {
            cancel.check()?;
            if !matches!(argument,LessMixinArgument::Value(value) if plain_value(value)) {
                return Ok(None);
            }
        }
        let Some(count) = self.names.get(name).copied().flatten() else {
            return Ok(None);
        };
        Ok((count == arguments.args.len()).then_some(count))
    }
}

struct Projection<'a> {
    facts: Vec<SyntaxFact>,
    source: &'a str,
    base: usize,
    file: FileId,
    maximum: usize,
    max_depth: usize,
    depth: usize,
    partial: bool,
    coordinates: &'a source_projection::View<'a>,
}

struct Output {
    facts: Vec<SyntaxFact>,
    depth: usize,
    partial: bool,
}

impl Projection<'_> {
    fn push(
        &mut self,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        start: usize,
        end: usize,
        depth: usize,
        label: &str,
    ) -> Result<u64, AdapterError> {
        if self.facts.len() >= self.maximum {
            return Err(failure("less-syntax-node-limit"));
        }
        if depth > self.max_depth {
            return Err(failure("less-syntax-depth-limit"));
        }
        if self.source.get(start..end).is_none() {
            return Err(failure("less-source-span-invalid"));
        }
        let id = u64::try_from(self.facts.len())
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| failure("less-source-accounting"))?;
        let span = SourceSpan::new(
            self.file,
            u64::try_from(self.coordinates.physical(start)?)
                .map_err(|_| failure("less-source-offset"))?,
            u64::try_from(self.coordinates.physical(end)?)
                .map_err(|_| failure("less-source-offset"))?,
        )
        .map_err(|_| failure("less-source-span-invalid"))?;
        self.facts
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.facts.push(SyntaxFact::new(
            id,
            parent,
            kind,
            span,
            depth,
            SyntaxKindLabel::new(label).expect("fixed Less syntax label"),
        ));
        self.depth = self.depth.max(depth);
        Ok(id)
    }
    fn native(
        &mut self,
        parent: u64,
        kind: SyntaxFactKind,
        span: &Span,
        depth: usize,
        label: &str,
    ) -> Result<u64, AdapterError> {
        let start = span
            .start
            .checked_add(self.base)
            .ok_or_else(|| failure("less-source-offset"))?;
        let end = span
            .end
            .checked_add(self.base)
            .ok_or_else(|| failure("less-source-offset"))?;
        self.push(Some(parent), kind, start, end, depth, label)
    }
    fn unsupported(&mut self, parent: u64, span: &Span, depth: usize) -> Result<(), AdapterError> {
        self.native(
            parent,
            SyntaxFactKind::Signature,
            span,
            depth,
            "less.unsupported.construct",
        )?;
        Ok(())
    }
    fn declaration(
        &mut self,
        parent: u64,
        full: &Span,
        name: &Span,
        depth: usize,
        label: &str,
    ) -> Result<u64, AdapterError> {
        let id = self.native(parent, SyntaxFactKind::Declaration, full, depth, label)?;
        self.native(
            id,
            SyntaxFactKind::Signature,
            name,
            depth + 1,
            "less.name.signature",
        )?;
        self.native(
            id,
            SyntaxFactKind::Occurrence,
            name,
            depth + 1,
            "less.name.definition",
        )?;
        Ok(id)
    }
    fn mixin(
        &mut self,
        mixin: &LessMixinDefinition<'_>,
        root: u64,
        census: &Census<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancel.check()?;
        let Some(name) = mixin_name(&mixin.name) else {
            return self.unsupported(root, &mixin.span, 1);
        };
        let written = self
            .source
            .get(name.start + self.base..name.end + self.base)
            .ok_or_else(|| failure("less-source-span-invalid"))?;
        if census.names.get(written).copied().flatten().is_none() {
            return self.unsupported(root, &mixin.span, 1);
        }
        let owner = self.declaration(root, &mixin.span, name, 1, "less.mixin.declaration")?;
        self.native(
            owner,
            SyntaxFactKind::Signature,
            &Span {
                start: mixin.span.start,
                end: mixin.block.span.start,
            },
            2,
            "less.mixin.header.signature",
        )?;
        for parameter in &mixin.params.params {
            cancel.check()?;
            if let Some(name) = parameter_name(parameter) {
                self.declaration(
                    owner,
                    parameter.span(),
                    &name.span,
                    2,
                    "less.parameter.declaration",
                )?;
            } else {
                self.unsupported(owner, parameter.span(), 2)?;
            }
        }
        let block = self.native(
            owner,
            SyntaxFactKind::Scope,
            &mixin.block.span,
            2,
            "less.block.scope",
        )?;
        for statement in &mixin.block.statements {
            cancel.check()?;
            if let Statement::LessMixinCall(call) = statement {
                if let [callee] = call.callee.children.as_slice()
                    && let Some(name) = mixin_name(&callee.name)
                {
                    self.native(
                        block,
                        SyntaxFactKind::Occurrence,
                        name,
                        3,
                        if census.closed
                            && census
                                .target(call, &self.source[self.base..], cancel)?
                                .is_some()
                        {
                            "less.direct_declared_mixin.call"
                        } else {
                            "less.unresolved_mixin.call"
                        },
                    )?;
                } else {
                    self.unsupported(block, &call.span, 3)?;
                }
            } else {
                self.unsupported(block, statement.span(), 3)?;
            }
        }
        if mixin.guard.is_some() {
            self.unsupported(owner, &mixin.span, 2)?;
        }
        Ok(())
    }
}

fn failure(code: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("fixed Less diagnostic"),
    }
}
