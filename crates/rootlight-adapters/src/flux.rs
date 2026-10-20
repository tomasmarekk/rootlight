//! Project exact official Flux AST witnesses into a bounded source-only slice.
//! Only ordinary written function assignments and their formals become entities.
//! A Read target requires a native value leaf and a unique formal in that owner.

use crate::source_projection;

use flux_source_parser::{
    ast::{
        self,
        walk::{self, Node, Visitor},
    },
    control::{Control, Limits, Stop},
    controlled_parser::{ControlledParser, SourcePolicy},
};
use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, CoverageReport, DescriptorError, DiagnosticCode,
    DomainCoverage, EncodingId, LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider,
    ParseReport, ParseRequest, ResourceUsage, SinkError, StreamEnd, SyntaxFact, SyntaxFactBatch,
    SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId, content_hash};
use rootlight_ir::{AnalysisTier, CoverageStatus, DiagnosticSeverity, FactDomain, SourceSpan};

const HARD_NODES: usize = 2_097_152;
const HARD_DEPTH: usize = 128;

/// A cooperative official-parser bridge with explicit partial source coverage.
pub struct FluxParseProvider {
    capabilities: ParseCapabilities,
}

impl FluxParseProvider {
    /// Identifies this bridge; producer sources have an additional pinned manifest.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        content_hash(include_bytes!("flux.rs"))
    }

    /// Creates a bounded UTF-8 and BOM-backed UTF-16 frontend.
    ///
    /// # Errors
    /// Returns a descriptor error for invalid ceilings.
    ///
    /// # Panics
    /// Panics only if the fixed language identifier violates the SDK contract.
    pub fn new(bytes: usize, nodes: usize) -> Result<Self, DescriptorError> {
        Ok(Self {
            capabilities: ParseCapabilities::new(
                vec![LanguageId::new("flux").expect("fixed Flux label")],
                vec![
                    EncodingId::utf8(),
                    EncodingId::new("utf-16le").expect("fixed UTF-16LE label"),
                    EncodingId::new("utf-16be").expect("fixed UTF-16BE label"),
                ],
                bytes,
                nodes.min(HARD_NODES),
                HARD_DEPTH,
                0,
                false,
                true,
                true,
                1,
                MemoryEnforcement::Unavailable,
            )?,
        })
    }

    /// Counts the identical guarded projection used by `parse`.
    ///
    /// # Errors
    /// Returns exact context, cancellation, native control or allocation errors.
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
        if request.language().as_str() != "flux"
            || !matches!(
                request.encoding().as_str(),
                "utf-8" | "utf-16le" | "utf-16be"
            )
            || !request.included_ranges().is_empty()
        {
            return Err(failure("flux-source-context-unsupported"));
        }
        let raw = request.source().bytes();
        if raw.len() > self.capabilities.max_source_bytes() {
            return Err(failure("flux-source-byte-limit"));
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
        let physical = view.text();
        let text = physical.strip_prefix('\u{feff}').unwrap_or(physical);
        let base = physical.len() - text.len();
        let maximum = request
            .limits()
            .max_syntax_nodes()
            .min(self.capabilities.max_syntax_nodes());
        let depth = request
            .limits()
            .max_syntax_depth()
            .min(self.capabilities.max_syntax_depth());
        let limits = Limits {
            source_bytes: self.capabilities.max_source_bytes(),
            transitions: scaled(maximum, 128)?,
            tokens: scaled(maximum, 8)?,
            positions: scaled(maximum, 8)?,
            comments: scaled(maximum, 8)?,
            parser_depth: depth,
            ast_nodes: scaled(maximum, 16)?,
        };
        let mut cancelled = |_| cancel.check().is_err();
        let control = Control::new(limits, &mut cancelled);
        let mut parser = ControlledParser::new(text, control, SourcePolicy::LocationsOnly)
            .map_err(|stop| native_failure(stop, cancel))?;
        let parsed = parser.parse_file(String::new());
        cancel.check()?;
        let (tree, unavailable) = match parsed {
            Ok(tree) => (Some(tree), None),
            Err(Stop::UnsupportedRegexValidation) => {
                (None, Some("flux.regex_validation_unavailable"))
            }
            Err(Stop::UnsupportedTimePrecision) => (None, Some("flux.time_precision_unavailable")),
            // The provider's native stack ceiling declines analysis, while a
            // stricter caller depth remains a refused resource request.
            Err(Stop::Depth)
                if request.limits().max_syntax_depth() >= self.capabilities.max_syntax_depth() =>
            {
                (None, Some("flux.native_depth_unavailable"))
            }
            Err(stop) => return Err(native_failure(stop, cancel)),
        };
        let mut output = Projection::new(
            &view,
            base,
            request.source().source_ref().span().file(),
            maximum,
            depth,
            scaled(maximum, 16)?,
            cancel,
        )?;
        let mut native_nodes = 0;
        let mut native_depth = 0;
        let recovery = if let Some(tree) = &tree {
            let mut audit = Audit {
                remaining: scaled(maximum, 16)?,
                visited: 0,
                observed_depth: 0,
                depth: 0,
                maximum_depth: depth,
                cancel,
                failure: None,
                recovered: false,
            };
            walk::walk(&mut audit, Node::File(tree));
            cancel.check()?;
            native_nodes = audit.visited;
            native_depth = audit.observed_depth;
            match audit.failure {
                // The AST walk has a separate ancestry bound from the parser.
                // Only the declared hard bound declines analysis; caller work,
                // cancellation and explicitly smaller depths remain errors.
                Some(AuditFailure::Depth) if depth == HARD_DEPTH => {
                    Some("flux.native_depth_unavailable")
                }
                Some(error) => return Err(error.into_error()),
                None => audit.recovered.then_some("flux.native_recovery"),
            }
        } else {
            unavailable
        };
        let root = output.push(
            None,
            SyntaxFactKind::Root,
            0,
            physical.len(),
            0,
            "flux.source_file",
        )?;
        output.push(
            Some(root),
            SyntaxFactKind::Signature,
            0,
            physical.len(),
            1,
            if recovery.is_some() {
                "flux.partial_parse.signature"
            } else {
                "flux.clean_parse.signature"
            },
        )?;
        if let Some(reason) = recovery {
            // The native Stop carries no token range. Full-file uncertainty is
            // honest physical provenance; a guessed lexical range is not.
            output.push(
                Some(root),
                SyntaxFactKind::ErrorRecovery,
                0,
                physical.len(),
                1,
                reason,
            )?;
            output.partial = true;
        } else if let Some(tree) = &tree {
            for statement in &tree.body {
                output.step()?;
                if let ast::Statement::Variable(assignment) = statement
                    && let ast::Expression::Function(function) = &assignment.init
                    && assignment.base.attributes.is_empty()
                    && function.base.attributes.is_empty()
                {
                    output.function(root, assignment, function)?;
                } else {
                    output.gap(root, &statement.base().location, 1)?;
                }
            }
        }
        cancel.check()?;
        let mut output = output.finish();
        output.native_nodes = native_nodes;
        output.depth = output.depth.max(native_depth);
        Ok(output)
    }
}

impl ParseProvider for FluxParseProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancel: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let output = self.project(request, cancel)?;
        let batch_records = request
            .limits()
            .syntax_stream()
            .batch()
            .max_records()
            .min(32);
        let depth_unavailable = output.facts.iter().any(|fact| {
            fact.kind() == SyntaxFactKind::ErrorRecovery
                && fact.syntax_kind().as_str() == "flux.native_depth_unavailable"
        });
        for (batch_index, chunk) in output.facts.chunks(batch_records).enumerate() {
            cancel.check()?;
            let mut facts = Vec::new();
            facts
                .try_reserve_exact(chunk.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            facts.extend_from_slice(chunk);
            let mut diagnostics = Vec::new();
            if depth_unavailable && batch_index == 0 {
                diagnostics
                    .try_reserve_exact(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                diagnostics.push(AdapterDiagnostic::new(
                    DiagnosticCode::new("flux-native-depth-unavailable").expect("fixed Flux code"),
                    DiagnosticSeverity::Warning,
                    Some(request.source().source_ref().clone()),
                    CoverageStatus::Unknown,
                ));
            }
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), facts, diagnostics),
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
                if output.partial {
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
                if output.partial {
                    CoverageStatus::Unknown
                } else {
                    CoverageStatus::Bounded
                },
                bytes,
                bytes,
                usize::from(depth_unavailable),
                domains,
            )?,
            ResourceUsage::new(
                bytes,
                output.facts.len(),
                output.native_nodes,
                output.depth,
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

enum AuditFailure {
    Depth,
    Work,
    Cancelled(rootlight_cancel::Cancelled),
}

impl AuditFailure {
    fn into_error(self) -> AdapterError {
        match self {
            Self::Depth => failure("flux-native-audit-depth-limit"),
            Self::Work => failure("flux-native-audit-work-limit"),
            Self::Cancelled(error) => error.into(),
        }
    }
}

struct Audit<'a> {
    remaining: usize,
    visited: usize,
    observed_depth: usize,
    depth: usize,
    maximum_depth: usize,
    cancel: &'a Cancellation,
    failure: Option<AuditFailure>,
    recovered: bool,
}

impl<'a> Visitor<'a> for Audit<'_> {
    fn visit(&mut self, node: Node<'a>) -> bool {
        if self.failure.is_some() {
            return false;
        }
        if let Err(error) = self.cancel.check() {
            self.failure = Some(AuditFailure::Cancelled(error));
            return false;
        }
        if self.remaining == 0 {
            self.failure = Some(AuditFailure::Work);
            return false;
        }
        if self.depth >= self.maximum_depth {
            self.failure = Some(AuditFailure::Depth);
            return false;
        }
        self.remaining -= 1;
        self.visited += 1;
        self.depth += 1;
        self.observed_depth = self.observed_depth.max(self.depth);
        self.recovered |=
            !node.base().errors.is_empty() || matches!(node, Node::BadExpr(_) | Node::BadStmt(_));
        true
    }
    fn done(&mut self, _: Node<'a>) {
        self.depth = self.depth.saturating_sub(1);
    }
}

struct Output {
    facts: Vec<SyntaxFact>,
    native_nodes: usize,
    depth: usize,
    partial: bool,
}

struct Projection<'a> {
    source: &'a str,
    base: usize,
    lines: Vec<usize>,
    file: FileId,
    facts: Vec<SyntaxFact>,
    maximum: usize,
    maximum_depth: usize,
    depth: usize,
    work: usize,
    cancel: &'a Cancellation,
    partial: bool,
    coordinates: &'a source_projection::View<'a>,
}

impl<'a> Projection<'a> {
    fn new(
        coordinates: &'a source_projection::View<'a>,
        base: usize,
        file: FileId,
        maximum: usize,
        maximum_depth: usize,
        work: usize,
        cancel: &'a Cancellation,
    ) -> Result<Self, AdapterError> {
        let source = coordinates.text();
        let mut result = Self {
            source,
            base,
            lines: Vec::new(),
            file,
            facts: Vec::new(),
            maximum,
            maximum_depth,
            depth: 0,
            work,
            cancel,
            partial: false,
            coordinates,
        };
        result
            .lines
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        result.lines.push(base);
        for (offset, byte) in source.as_bytes()[base..].iter().enumerate() {
            if offset.is_multiple_of(256) {
                result.step()?;
            }
            if *byte == b'\n' {
                result.step()?;
                if result.lines.len() >= scaled(maximum, 8)? {
                    return Err(failure("flux-projection-position-limit"));
                }
                result
                    .lines
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                result.lines.push(base + offset + 1);
            }
        }
        Ok(result)
    }
    fn finish(self) -> Output {
        Output {
            facts: self.facts,
            native_nodes: 0,
            depth: self.depth,
            partial: self.partial,
        }
    }
    fn step(&mut self) -> Result<(), AdapterError> {
        self.cancel.check()?;
        self.work = self
            .work
            .checked_sub(1)
            .ok_or_else(|| failure("flux-projection-work-limit"))?;
        Ok(())
    }
    fn offset(&self, position: ast::Position) -> Result<usize, AdapterError> {
        let line_index = usize::try_from(position.line)
            .ok()
            .and_then(|line| line.checked_sub(1))
            .ok_or_else(|| failure("flux-native-position"))?;
        let line = self
            .lines
            .get(line_index)
            .copied()
            .ok_or_else(|| failure("flux-native-position"))?;
        let offset = usize::try_from(position.column)
            .ok()
            .and_then(|column| column.checked_sub(1))
            .and_then(|column| line.checked_add(column))
            .ok_or_else(|| failure("flux-native-position"))?;
        if offset < self.base
            || offset > self.source.len()
            || !self.source.is_char_boundary(offset)
            || self
                .lines
                .get(line_index + 1)
                .is_some_and(|next| offset >= *next)
        {
            return Err(failure("flux-native-position"));
        }
        Ok(offset)
    }
    fn range(&self, location: &ast::SourceLocation) -> Result<(usize, usize), AdapterError> {
        let start = self.offset(location.start)?;
        let end = self.offset(location.end)?;
        if end < start {
            return Err(failure("flux-native-position"));
        }
        Ok((start, end))
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
        self.step()?;
        if self.facts.len() >= self.maximum {
            return Err(failure("flux-syntax-node-limit"));
        }
        if depth > self.maximum_depth {
            return Err(failure("flux-syntax-depth-limit"));
        }
        if self.source.get(start..end).is_none() {
            return Err(failure("flux-source-span-invalid"));
        }
        let id = u64::try_from(self.facts.len())
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| failure("flux-source-accounting"))?;
        let span = SourceSpan::new(
            self.file,
            u64::try_from(self.coordinates.physical(start)?)
                .map_err(|_| failure("flux-source-offset"))?,
            u64::try_from(self.coordinates.physical(end)?)
                .map_err(|_| failure("flux-source-offset"))?,
        )
        .map_err(|_| failure("flux-source-span-invalid"))?;
        self.facts
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.facts.push(SyntaxFact::new(
            id,
            parent,
            kind,
            span,
            depth,
            SyntaxKindLabel::new(label).expect("fixed Flux label"),
        ));
        self.depth = self.depth.max(depth);
        Ok(id)
    }
    fn native(
        &mut self,
        parent: u64,
        kind: SyntaxFactKind,
        location: &ast::SourceLocation,
        depth: usize,
        label: &str,
    ) -> Result<u64, AdapterError> {
        let (start, end) = self.range(location)?;
        self.push(Some(parent), kind, start, end, depth, label)
    }
    fn gap(
        &mut self,
        parent: u64,
        location: &ast::SourceLocation,
        depth: usize,
    ) -> Result<(), AdapterError> {
        self.native(
            parent,
            SyntaxFactKind::Signature,
            location,
            depth,
            "flux.construct_unavailable.expression",
        )?;
        Ok(())
    }
    fn identity(
        &mut self,
        parent: u64,
        base: &ast::BaseNode,
        name: &ast::Identifier,
        depth: usize,
        label: &str,
    ) -> Result<u64, AdapterError> {
        let (name_start, name_end) = self.range(&name.base.location)?;
        let (start, end) = self.range(&base.location)?;
        if name_start < start
            || name_end > end
            || self.source.get(name_start..name_end) != Some(name.name.as_str())
        {
            return Err(failure("flux-native-name-proof"));
        }
        let id = self.push(
            Some(parent),
            SyntaxFactKind::Declaration,
            start,
            end,
            depth,
            label,
        )?;
        self.push(
            Some(id),
            SyntaxFactKind::Occurrence,
            name_start,
            name_end,
            depth + 1,
            "flux.name.definition",
        )?;
        Ok(id)
    }
    fn function(
        &mut self,
        root: u64,
        assignment: &ast::VariableAssgn,
        function: &ast::FunctionExpr,
    ) -> Result<(), AdapterError> {
        let owner = self.identity(
            root,
            &assignment.base,
            &assignment.id,
            1,
            "flux.function.declaration",
        )?;
        let (start, _) = self.range(&assignment.base.location)?;
        let (body_start, _) = self.range(&function.body.base().location)?;
        self.push(
            Some(owner),
            SyntaxFactKind::Signature,
            start,
            body_start,
            2,
            "flux.header.signature",
        )?;
        let mut plain = true;
        for (index, formal) in function.params.iter().enumerate() {
            self.step()?;
            let ast::PropertyKey::Identifier(name) = &formal.key else {
                return self.gap(owner, &function.base.location, 2);
            };
            if !formal.base.attributes.is_empty() || !name.base.attributes.is_empty() {
                return self.gap(owner, &function.base.location, 2);
            }
            for previous in &function.params[..index] {
                self.step()?;
                if let ast::PropertyKey::Identifier(other) = &previous.key
                    && self.same_name(&other.name, &name.name)?
                {
                    return self.gap(owner, &function.base.location, 2);
                }
            }
        }
        for formal in &function.params {
            self.step()?;
            let ast::PropertyKey::Identifier(name) = &formal.key else {
                return Err(failure("flux-native-parameter-invariant"));
            };
            let parameter =
                self.identity(owner, &formal.base, name, 2, "flux.parameter.declaration")?;
            self.native(
                parameter,
                SyntaxFactKind::Signature,
                &name.base.location,
                3,
                "flux.header.signature",
            )?;
            if formal.value.is_some() {
                plain = false;
                self.gap(owner, &formal.base.location, 2)?;
            }
        }
        let ast::FunctionBody::Expr(body) = &function.body else {
            return self.gap(owner, &function.body.base().location, 2);
        };
        let mut values = Vec::new();
        if !plain || !self.values(body, &mut values, 0)? {
            return self.gap(owner, &body.base().location, 2);
        }
        for value in values {
            self.step()?;
            let mut target = None;
            for formal in &function.params {
                self.step()?;
                if let ast::PropertyKey::Identifier(name) = &formal.key
                    && self.same_name(&name.name, &value.name)?
                {
                    if target.is_some() {
                        return Err(failure("flux-native-parameter-ambiguity"));
                    }
                    target = Some(name);
                }
            }
            let (value_start, value_end) = self.range(&value.base.location)?;
            if self.source.get(value_start..value_end) != Some(value.name.as_str()) {
                return Err(failure("flux-native-name-proof"));
            }
            self.native(
                owner,
                SyntaxFactKind::Occurrence,
                &value.base.location,
                2,
                if target.is_some() {
                    "flux.parameter_read.reference"
                } else {
                    "flux.unresolved_read.reference"
                },
            )?;
            if let Some(target) = target {
                let (target_start, _) = self.range(&target.base.location)?;
                let (_, read_end) = self.range(&value.base.location)?;
                self.push(
                    Some(owner),
                    SyntaxFactKind::Signature,
                    target_start,
                    read_end,
                    2,
                    "flux.parameter_target.signature",
                )?;
            }
        }
        Ok(())
    }
    fn same_name(&mut self, left: &str, right: &str) -> Result<bool, AdapterError> {
        self.step()?;
        if left.len() != right.len() {
            return Ok(false);
        }
        for (left, right) in left.bytes().zip(right.bytes()) {
            self.step()?;
            if left != right {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn values<'b>(
        &mut self,
        expression: &'b ast::Expression,
        values: &mut Vec<&'b ast::Identifier>,
        depth: usize,
    ) -> Result<bool, AdapterError> {
        self.step()?;
        if depth >= self.maximum_depth {
            return Err(failure("flux-projection-depth-limit"));
        }
        if !expression.base().attributes.is_empty() {
            return Ok(false);
        }
        use ast::Expression as E;
        Ok(match expression {
            E::Identifier(name) => {
                self.push_value(name, values)?;
                true
            }
            E::Call(call) => {
                let mut supported = self.values(&call.callee, values, depth + 1)?;
                for argument in &call.arguments {
                    supported &= self.values(argument, values, depth + 1)?;
                }
                supported
            }
            E::Member(member) => self.values(&member.object, values, depth + 1)?,
            E::Paren(paren) => self.values(&paren.expression, values, depth + 1)?,
            E::Binary(binary) => {
                self.values(&binary.left, values, depth + 1)?
                    & self.values(&binary.right, values, depth + 1)?
            }
            E::Logical(logical) => {
                self.values(&logical.left, values, depth + 1)?
                    & self.values(&logical.right, values, depth + 1)?
            }
            E::Unary(unary) => self.values(&unary.argument, values, depth + 1)?,
            E::Object(object) if object.with.is_none() => {
                let mut supported = true;
                for property in &object.properties {
                    self.step()?;
                    if let Some(value) = &property.value {
                        supported &= self.values(value, values, depth + 1)?;
                    } else if let ast::PropertyKey::Identifier(name) = &property.key
                        && name.base.attributes.is_empty()
                    {
                        // The native shorthand key also supplies its value;
                        // explicit keys remain labels when a value is present.
                        self.push_value(name, values)?;
                    } else {
                        supported = false;
                    }
                }
                supported
            }
            E::Integer(_)
            | E::Float(_)
            | E::StringLit(_)
            | E::Duration(_)
            | E::Uint(_)
            | E::Boolean(_)
            | E::DateTime(_)
            | E::Label(_) => true,
            _ => false,
        })
    }

    fn push_value<'b>(
        &self,
        name: &'b ast::Identifier,
        values: &mut Vec<&'b ast::Identifier>,
    ) -> Result<(), AdapterError> {
        if values.len() >= self.maximum {
            return Err(failure("flux-projection-value-limit"));
        }
        values
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        values.push(name);
        Ok(())
    }
}

fn scaled(value: usize, factor: usize) -> Result<usize, AdapterError> {
    value
        .checked_mul(factor)
        .ok_or_else(|| failure("flux-control-accounting"))
}

fn native_failure(stop: Stop, cancel: &Cancellation) -> AdapterError {
    if let Err(error) = cancel.check() {
        return error.into();
    }
    match stop {
        Stop::Allocation => SinkError::AllocationFailed.into(),
        Stop::Work => failure("flux-native-work-limit"),
        Stop::Tokens => failure("flux-native-token-limit"),
        Stop::Positions => failure("flux-native-position-limit"),
        Stop::Comments => failure("flux-native-comment-limit"),
        Stop::Depth => failure("flux-native-depth-limit"),
        Stop::Nodes => failure("flux-native-node-limit"),
        Stop::SourceLength => failure("flux-native-source-limit"),
        _ => failure("flux-native-control-invariant"),
    }
}

fn failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("fixed Flux code"),
    }
}

#[cfg(test)]
mod audit_depth_tests {
    use super::*;

    fn original_tree() -> ast::File {
        let raw = include_bytes!("../tests/fixtures/flux/original-stack_overflow_2.flux");
        let mut cancelled = |_| false;
        let control = Control::new(
            Limits {
                source_bytes: 262_144,
                transitions: 16_384 * 128,
                tokens: 16_384 * 8,
                positions: 16_384 * 8,
                comments: 16_384 * 8,
                parser_depth: HARD_DEPTH,
                ast_nodes: 16_384 * 16,
            },
            &mut cancelled,
        );
        let mut parser = ControlledParser::new(
            std::str::from_utf8(raw).unwrap(),
            control,
            SourcePolicy::LocationsOnly,
        )
        .unwrap();
        parser.parse_file(String::new()).unwrap()
    }

    #[test]
    fn parsed_original_reaches_audit_depth_with_work_remaining() {
        let tree = original_tree();
        let cancel = Cancellation::new();
        let mut audit = Audit {
            remaining: 16_384 * 16,
            visited: 0,
            observed_depth: 0,
            depth: 0,
            maximum_depth: HARD_DEPTH,
            cancel: &cancel,
            failure: None,
            recovered: false,
        };
        walk::walk(&mut audit, Node::File(&tree));
        assert!(matches!(audit.failure, Some(AuditFailure::Depth)));
        assert!(audit.remaining > 0);
        assert!(audit.visited >= HARD_DEPTH);
        assert_eq!(audit.observed_depth, HARD_DEPTH);
        assert!(!audit.recovered);
    }

    #[test]
    fn audit_work_and_cancel_precede_depth_and_stay_typed_refusals() {
        let tree = original_tree();
        let cancel = Cancellation::new();
        for (remaining, depth, maximum_depth, code) in [
            (0, HARD_DEPTH, HARD_DEPTH, "flux-native-audit-work-limit"),
            (
                16,
                HARD_DEPTH - 1,
                HARD_DEPTH - 1,
                "flux-native-audit-depth-limit",
            ),
        ] {
            let mut audit = Audit {
                remaining,
                visited: 0,
                observed_depth: 0,
                depth,
                maximum_depth,
                cancel: &cancel,
                failure: None,
                recovered: false,
            };
            assert!(!audit.visit(Node::File(&tree)));
            assert_eq!(audit.remaining, remaining);
            assert_eq!(audit.visited, 0);
            assert!(matches!(audit.failure.unwrap().into_error(),
                AdapterError::ProviderFailed { code: actual } if actual.as_str() == code));
        }
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut audit = Audit {
            remaining: 0,
            visited: 0,
            observed_depth: 0,
            depth: HARD_DEPTH,
            maximum_depth: HARD_DEPTH,
            cancel: &cancelled,
            failure: None,
            recovered: false,
        };
        assert!(!audit.visit(Node::File(&tree)));
        assert_eq!(audit.visited, 0);
        assert_eq!(audit.remaining, 0);
        assert_eq!(
            audit.failure.unwrap().into_error(),
            AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            }
        );
    }
}
