//! Distinct indented-Sass AST projection with exact physical source witnesses.
//! Only native declarations, selectors and property leaves become identities;
//! source-order variable reads require a clean parse and an unblocked context.

mod control;

use std::collections::{HashMap, HashSet};

use raffia::{ParserBuilder, Syntax, ast::*, pos::Spanned};
use rootlight_adapter_sdk::{
    AdapterError, CoverageReport, DescriptorError, DiagnosticCode, DomainCoverage, EncodingId,
    LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider, ParseReport, ParseRequest,
    ResourceUsage, SinkError, StreamEnd, SyntaxFact, SyntaxFactBatch, SyntaxFactKind,
    SyntaxFactSink, SyntaxKindLabel,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId, content_hash};
use rootlight_ir::{AnalysisTier, CoverageStatus, FactDomain, SourceSpan};

const MAX_SYNTAX_NODES: usize = 2_097_152;

/// A guarded AST frontend for ordinary authored indented-Sass constructs.
pub struct SassParseProvider {
    capabilities: ParseCapabilities,
}

impl SassParseProvider {
    /// Returns the first-party bridge content identity; vendored frontend inputs
    /// are additionally pinned by the retained build/source manifest.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("sass.rs"))
    }

    /// Creates a UTF-8 frontend with explicit source, syntax and depth ceilings.
    ///
    /// # Errors
    /// Returns a descriptor error for zero ceilings. UTF-16 stays unsupported
    /// until a separately reviewed physical-coordinate bridge is available.
    ///
    /// # Panics
    /// Panics only if the fixed `sass` language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        Ok(Self {
            capabilities: ParseCapabilities::new(
                vec![LanguageId::new("sass").expect("fixed Sass label")],
                vec![EncodingId::utf8()],
                max_source_bytes,
                max_syntax_nodes.min(MAX_SYNTAX_NODES),
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

    /// Counts the exact same staged facts as parsing, under the same guards.
    ///
    /// # Errors
    /// Returns context, encoding, source-budget, parser-control, allocation or
    /// cancellation failure. Native syntax rejection returns only exact source
    /// recovery facts with unknown analysis coverage and no binding certificate.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        Ok(self.project(request, cancellation)?.facts.len())
    }

    fn project(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<Output, AdapterError> {
        cancellation.check()?;
        if request.language().as_str() != "sass"
            || request.encoding().as_str() != "utf-8"
            || !request.included_ranges().is_empty()
        {
            return Err(failure("sass-source-context-unsupported"));
        }
        let raw = request.source().bytes();
        if raw.len() > self.capabilities.max_source_bytes() {
            return Err(failure("sass-source-byte-limit"));
        }
        let physical = std::str::from_utf8(raw).map_err(|_| failure("sass-invalid-utf8"))?;
        let text = physical.strip_prefix('\u{feff}').unwrap_or(physical);
        let base = physical.len() - text.len();
        if text.contains('\0') {
            return Err(failure("sass-invalid-source-nul"));
        }
        let initial_indentation = text.starts_with([' ', '\t']);
        let maximum = request
            .limits()
            .max_syntax_nodes()
            .min(self.capabilities.max_syntax_nodes());
        let maximum_depth = request
            .limits()
            .max_syntax_depth()
            .min(self.capabilities.max_syntax_depth());
        let control = control::Control::new(maximum, maximum_depth, cancellation)?;
        let mut parser = ParserBuilder::new(text)
            .syntax(Syntax::Sass)
            .control(control.hook())
            .build();
        let parsed = parser.parse::<Stylesheet>();
        control.check()?;
        for error in parser.recoverable_errors() {
            cancellation.check()?;
            if matches!(error.kind, raffia::error::ErrorKind::ControlledStop) {
                return Err(failure("sass-parser-controlled-stop"));
            }
        }
        let (tree, recovery) = match parsed {
            Ok(_) if initial_indentation => (
                None,
                Some((
                    "sass.initial_indentation_unavailable",
                    raffia::pos::Span {
                        start: 0,
                        end: text
                            .bytes()
                            .take_while(|byte| matches!(byte, b' ' | b'\t'))
                            .count(),
                    },
                )),
            ),
            Ok(tree) => match parser.recoverable_errors().first() {
                Some(error) => (None, Some(("sass.parse_recovery", error.span.clone()))),
                None => (Some(tree), None),
            },
            Err(error) => (None, Some(native_syntax_rejection(error)?)),
        };
        let partial = recovery.is_some();
        let file = request.source().source_ref().span().file();
        let mut projection = Projection {
            facts: Vec::new(),
            variables: HashMap::new(),
            source: physical,
            base,
            file,
            maximum,
            maximum_depth,
            depth: 0,
            declarations: 0,
            partial,
            semantic_barrier: false,
        };
        let root = projection.push(
            None,
            SyntaxFactKind::Root,
            0,
            raw.len(),
            0,
            "sass.source_file",
        )?;
        projection.push(
            Some(root),
            SyntaxFactKind::Signature,
            0,
            raw.len(),
            1,
            if partial {
                "sass.partial_parse.signature"
            } else {
                "sass.clean_parse.signature"
            },
        )?;
        if let Some((label, span)) = recovery {
            projection.native(root, SyntaxFactKind::ErrorRecovery, &span, 1, label)?;
        } else if let Some(tree) = tree {
            projection.statements(&tree.statements, root, 1, false, cancellation)?;
        }
        cancellation.check()?;
        Ok(projection.finish())
    }
}

impl ParseProvider for SassParseProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let projection = self.project(request, cancellation)?;
        let batch_records = request
            .limits()
            .syntax_stream()
            .batch()
            .max_records()
            .min(32);
        for batch in projection.facts.chunks(batch_records) {
            cancellation.check()?;
            let mut facts = Vec::new();
            facts
                .try_reserve_exact(batch.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            facts.extend_from_slice(batch);
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), facts, Vec::new()),
                cancellation,
            )?;
        }
        let usage = sink.staged_usage();
        let bytes = request.source().bytes().len();
        let mut domains = Vec::new();
        domains
            .try_reserve_exact(if projection.partial { 4 } else { 2 })
            .map_err(|_| SinkError::AllocationFailed)?;
        domains.push(DomainCoverage::new(
            FactDomain::Files,
            CoverageStatus::Complete,
            1,
            1,
            0,
        )?);
        if projection.partial {
            for domain in [
                FactDomain::Entities,
                FactDomain::Occurrences,
                FactDomain::Relations,
            ] {
                domains.push(DomainCoverage::new(
                    domain,
                    CoverageStatus::Unknown,
                    0,
                    0,
                    0,
                )?);
            }
        } else {
            domains.push(DomainCoverage::new(
                FactDomain::Entities,
                CoverageStatus::Bounded,
                projection.declarations,
                projection.declarations,
                0,
            )?);
        }
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            if projection.partial {
                CoverageStatus::Unknown
            } else {
                CoverageStatus::Bounded
            },
            bytes,
            bytes,
            0,
            domains,
        )?;
        Ok(ParseReport::new(
            coverage,
            ResourceUsage::new(
                bytes,
                projection.facts.len(),
                projection.facts.len(),
                projection.depth,
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

struct Projection<'a> {
    facts: Vec<SyntaxFact>,
    variables: HashMap<u64, HashSet<String>>,
    source: &'a str,
    base: usize,
    file: FileId,
    maximum: usize,
    maximum_depth: usize,
    depth: usize,
    declarations: usize,
    partial: bool,
    semantic_barrier: bool,
}

struct Output {
    facts: Vec<SyntaxFact>,
    depth: usize,
    declarations: usize,
    partial: bool,
}

impl Projection<'_> {
    fn finish(self) -> Output {
        Output {
            facts: self.facts,
            depth: self.depth,
            declarations: self.declarations,
            partial: self.partial,
        }
    }

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
            return Err(failure("sass-syntax-node-limit"));
        }
        if depth > self.maximum_depth {
            return Err(failure("sass-syntax-depth-limit"));
        }
        if self.source.get(start..end).is_none() {
            return Err(failure("sass-source-span-invalid"));
        }
        let id = u64::try_from(self.facts.len())
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| failure("sass-source-accounting"))?;
        let span = SourceSpan::new(
            self.file,
            u64::try_from(start).map_err(|_| failure("sass-source-offset"))?,
            u64::try_from(end).map_err(|_| failure("sass-source-offset"))?,
        )
        .map_err(|_| failure("sass-source-span-invalid"))?;
        self.facts
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.facts.push(SyntaxFact::new(
            id,
            parent,
            kind,
            span,
            depth,
            SyntaxKindLabel::new(label).expect("fixed Sass label"),
        ));
        self.depth = self.depth.max(depth);
        if kind == SyntaxFactKind::Declaration {
            self.declarations += 1;
        }
        Ok(id)
    }

    fn native(
        &mut self,
        parent: u64,
        kind: SyntaxFactKind,
        span: &raffia::pos::Span,
        depth: usize,
        label: &str,
    ) -> Result<u64, AdapterError> {
        let start = span
            .start
            .checked_add(self.base)
            .ok_or_else(|| failure("sass-source-offset"))?;
        let end = span
            .end
            .checked_add(self.base)
            .ok_or_else(|| failure("sass-source-offset"))?;
        self.push(Some(parent), kind, start, end, depth, label)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a declaration and its exact signature/definition witnesses are projected together"
    )]
    fn declaration(
        &mut self,
        scope: u64,
        full: &raffia::pos::Span,
        name: &raffia::pos::Span,
        depth: usize,
        kind: &str,
        signature: &str,
        definition: &str,
    ) -> Result<u64, AdapterError> {
        let declaration = self.native(scope, SyntaxFactKind::Declaration, full, depth, kind)?;
        self.native(
            declaration,
            SyntaxFactKind::Signature,
            name,
            depth + 1,
            signature,
        )?;
        self.native(
            declaration,
            SyntaxFactKind::Occurrence,
            name,
            depth + 1,
            definition,
        )?;
        Ok(declaration)
    }

    fn unsupported(
        &mut self,
        parent: u64,
        span: &raffia::pos::Span,
        depth: usize,
    ) -> Result<(), AdapterError> {
        self.native(
            parent,
            SyntaxFactKind::Signature,
            span,
            depth,
            "sass.unsupported.expression",
        )?;
        Ok(())
    }

    fn block(
        &mut self,
        block: &SimpleBlock<'_>,
        parent: u64,
        depth: usize,
        blocked: bool,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        let scope = self.native(
            parent,
            SyntaxFactKind::Scope,
            &block.span,
            depth,
            "sass.block.scope",
        )?;
        self.statements(&block.statements, scope, depth + 1, blocked, cancellation)
    }

    fn statements(
        &mut self,
        statements: &[Statement<'_>],
        scope: u64,
        depth: usize,
        blocked: bool,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        for statement in statements {
            cancellation.check()?;
            match statement {
                Statement::QualifiedRule(rule) => {
                    let declaration = self.declaration(
                        scope,
                        &rule.span,
                        &rule.selector.span,
                        depth,
                        "sass.style_rule.declaration",
                        "sass.selector.signature",
                        "sass.selector.definition",
                    )?;
                    let selector = self
                        .source
                        .get(
                            rule.selector.span.start + self.base
                                ..rule.selector.span.end + self.base,
                        )
                        .ok_or_else(|| failure("sass-source-span-invalid"))?;
                    if selector.contains("#{") {
                        self.semantic_barrier = true;
                        self.unsupported(scope, &rule.selector.span, depth)?;
                    }
                    self.block(&rule.block, declaration, depth + 1, blocked, cancellation)?;
                }
                Statement::Declaration(declaration) => {
                    let parent = if let InterpolableIdent::Literal(name) = &declaration.name {
                        self.declaration(
                            scope,
                            &declaration.span,
                            &name.span,
                            depth,
                            "sass.property.declaration",
                            "sass.name.signature",
                            "sass.name.definition",
                        )?
                    } else {
                        self.unsupported(scope, &declaration.span, depth)?;
                        scope
                    };
                    for value in &declaration.value {
                        self.value(value, parent, depth + 1, blocked, cancellation)?;
                    }
                }
                Statement::SassVariableDeclaration(declaration) => {
                    let raw_name = self
                        .source
                        .get(
                            declaration.name.span.start + self.base
                                ..declaration.name.span.end + self.base,
                        )
                        .ok_or_else(|| failure("sass-source-span-invalid"))?;
                    let uncertain = declaration.namespace.is_some()
                        || !declaration.flags.is_empty()
                        || raw_name.contains('\\');
                    if uncertain {
                        self.semantic_barrier = true;
                        self.native(
                            scope,
                            SyntaxFactKind::Occurrence,
                            &declaration.name.span,
                            depth,
                            "sass.assignment.write",
                        )?;
                        self.unsupported(scope, &declaration.span, depth)?;
                        self.value(&declaration.value, scope, depth + 1, true, cancellation)?;
                        continue;
                    }
                    let key = raw_name.replace('_', "-");
                    if !self.variables.contains_key(&scope) {
                        self.variables
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                    }
                    let names = self.variables.entry(scope).or_default();
                    let first = !names.contains(&key);
                    if first {
                        names
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        names.insert(key);
                    }
                    let parent = if first {
                        self.declaration(
                            scope,
                            &declaration.span,
                            &declaration.name.span,
                            depth,
                            "sass.variable.declaration",
                            "sass.name.signature",
                            "sass.name.definition",
                        )?
                    } else {
                        self.native(
                            scope,
                            SyntaxFactKind::Occurrence,
                            &declaration.name.span,
                            depth,
                            "sass.assignment.write",
                        )?;
                        scope
                    };
                    self.value(&declaration.value, parent, depth + 1, blocked, cancellation)?;
                }
                Statement::SassMixinStatement(mixin) => {
                    self.unsupported(scope, &mixin.span, depth)?;
                    let old = self.semantic_barrier;
                    self.block(&mixin.block, scope, depth, true, cancellation)?;
                    self.semantic_barrier = old;
                }
                Statement::SassIncludeStatement(include) => {
                    self.semantic_barrier = true;
                    self.unsupported(scope, &include.span, depth)?;
                    if let Some(block) = &include.block {
                        self.block(block, scope, depth, true, cancellation)?;
                    }
                }
                Statement::AtRule(rule) => {
                    self.unsupported(scope, &rule.span, depth)?;
                    self.semantic_barrier = true;
                    if let Some(block) = &rule.block {
                        self.block(block, scope, depth, true, cancellation)?;
                    }
                }
                _ => {
                    self.semantic_barrier = true;
                    self.unsupported(scope, statement.span(), depth)?;
                }
            }
        }
        Ok(())
    }

    fn value(
        &mut self,
        value: &ComponentValue<'_>,
        parent: u64,
        depth: usize,
        blocked: bool,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancellation.check()?;
        if depth > self.maximum_depth {
            return Err(failure("sass-value-depth-limit"));
        }
        self.depth = self.depth.max(depth);
        match value {
            ComponentValue::SassVariable(variable) => {
                self.native(
                    parent,
                    SyntaxFactKind::Occurrence,
                    &variable.span,
                    depth,
                    if blocked || self.semantic_barrier {
                        "sass.unresolved_read.reference"
                    } else {
                        "sass.plain_read.reference"
                    },
                )?;
            }
            ComponentValue::SassBinaryExpression(expression) => {
                self.value(&expression.left, parent, depth + 1, blocked, cancellation)?;
                self.value(&expression.right, parent, depth + 1, blocked, cancellation)?;
            }
            ComponentValue::SassUnaryExpression(expression) => {
                self.value(&expression.expr, parent, depth + 1, blocked, cancellation)?
            }
            ComponentValue::SassParenthesizedExpression(expression) => {
                self.value(&expression.expr, parent, depth + 1, blocked, cancellation)?
            }
            ComponentValue::SassList(list) => {
                for element in &list.elements {
                    self.value(element, parent, depth + 1, blocked, cancellation)?;
                }
            }
            ComponentValue::Number(_)
            | ComponentValue::Dimension(_)
            | ComponentValue::Percentage(_)
            | ComponentValue::HexColor(_)
            | ComponentValue::InterpolableIdent(InterpolableIdent::Literal(_))
            | ComponentValue::InterpolableStr(InterpolableStr::Literal(_))
            | ComponentValue::Delimiter(_) => {}
            _ => {
                self.semantic_barrier = true;
                self.unsupported(parent, value.span(), depth)?;
            }
        }
        Ok(())
    }
}

fn native_syntax_rejection(
    error: raffia::error::Error,
) -> Result<(&'static str, raffia::pos::Span), AdapterError> {
    let label = match error.kind {
        raffia::error::ErrorKind::ControlledStop => {
            return Err(failure("sass-parser-controlled-stop"));
        }
        raffia::error::ErrorKind::InvalidSassIndentation => "sass.rejected_indentation",
        _ => "sass.rejected_syntax",
    };
    Ok((label, error.span))
}

fn failure(value: &str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(value).expect("fixed Sass diagnostic"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_native_controlled_stop_is_never_syntax_recovery() {
        let error = raffia::error::Error {
            kind: raffia::error::ErrorKind::ControlledStop,
            span: raffia::pos::Span { start: 0, end: 0 },
        };
        assert!(
            matches!(native_syntax_rejection(error), Err(AdapterError::ProviderFailed { code })
            if code.as_str() == "sass-parser-controlled-stop")
        );
    }
}
