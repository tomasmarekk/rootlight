//! Iterative source-bound expressions, statement calls and standalone value applications.
//! Flat temporary nodes preserve precedence without recursive parsing or dropping.
//! The retained inventory is syntax only, not evaluation, type or binding evidence.

use std::ops::Range;

use super::{
    logical_whitespace_len,
    module::ModuleError,
    module_body::{ModuleScalarValue, ModuleScalarValueKind, atomic_value, radix_integer_prefix},
};

/// A complete simple-name assignment with a structured expression right-hand side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleExpressionAssignment {
    pub(super) statement: Range<usize>,
    pub(super) destination: Range<usize>,
    pub(super) expression: Range<usize>,
    pub(super) inventory: ModuleExpressionInventory,
}

impl ModuleExpressionAssignment {
    /// Returns the complete trimmed statement, including an optional Let prefix.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }

    /// Returns the exact simple destination name, excluding Let and trivia.
    #[must_use]
    pub fn destination(&self) -> Range<usize> {
        self.destination.clone()
    }

    /// Returns the complete expression, excluding its surrounding logical trivia.
    #[must_use]
    pub fn expression(&self) -> Range<usize> {
        self.expression.clone()
    }

    /// Returns the source inventory without evaluating or resolving its syntax.
    #[must_use]
    pub fn inventory(&self) -> &ModuleExpressionInventory {
        &self.inventory
    }
}

/// One explicit or implicit call with complete positional expression arguments.
///
/// The inventory's first application is the statement's outer callee; subsequent
/// applications are nested argument syntax. Consumers must not emit that first
/// record again as an implicit value application. Syntax does not prove a target,
/// formal-parameter correspondence, runtime types or successful evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleExpressionCall {
    statement: Range<usize>,
    callee: Range<usize>,
    arguments: Range<usize>,
    inventory: ModuleExpressionInventory,
}

impl ModuleExpressionCall {
    /// Returns the complete trimmed statement, including Call when authored.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }

    /// Returns the exact simple callee name, retaining authored brackets.
    #[must_use]
    pub fn callee(&self) -> Range<usize> {
        self.callee.clone()
    }

    /// Returns the authored argument bytes, retaining internal logical trivia.
    /// Explicit Call lists include their parentheses; implicit lists contain only
    /// their complete comma-separated expressions, without surrounding trivia.
    #[must_use]
    pub fn arguments(&self) -> Range<usize> {
        self.arguments.clone()
    }

    /// Returns complete outer positional expressions without surrounding trivia.
    #[must_use]
    pub fn argument_expressions(&self) -> &[Range<usize>] {
        self.inventory
            .applications
            .first()
            .map_or(&[], |application| application.argument_expressions())
    }

    /// Returns source records, with the statement's outer application at index zero.
    #[must_use]
    pub fn inventory(&self) -> &ModuleExpressionInventory {
        &self.inventory
    }
}

/// One complete source-written parenthesized value application used as a statement.
///
/// Unlike [`ModuleExpressionCall`], its outer callee is a value-application target.
/// Get/index syntax does not prove a procedure call, array access, formal-parameter
/// correspondence, runtime type or successful evaluation. The inventory starts
/// with the exact outer application, followed by its nested applications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleValueApplication {
    statement: Range<usize>,
    application: Range<usize>,
    callee: Range<usize>,
    arguments: Range<usize>,
    inventory: ModuleExpressionInventory,
}

impl ModuleValueApplication {
    /// Returns the complete statement, excluding a trailing comment or separator.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }

    /// Returns the callee through its authored closing parenthesis, without trailing trivia.
    #[must_use]
    pub fn application(&self) -> Range<usize> {
        self.application.clone()
    }

    /// Returns the exact simple value-application target, retaining authored brackets.
    #[must_use]
    pub fn callee(&self) -> Range<usize> {
        self.callee.clone()
    }

    /// Returns the complete authored argument list, including both parentheses.
    #[must_use]
    pub fn arguments(&self) -> Range<usize> {
        self.arguments.clone()
    }

    /// Returns positional expression spans inside the parentheses, excluding surrounding trivia.
    #[must_use]
    pub fn argument_expressions(&self) -> &[Range<usize>] {
        self.inventory
            .applications
            .first()
            .map_or(&[], |application| application.argument_expressions())
    }

    /// Returns the complete root and nested application syntax without resolving targets.
    #[must_use]
    pub fn inventory(&self) -> &ModuleExpressionInventory {
        &self.inventory
    }
}

/// Flat source records from one completely admitted expression or procedure call.
///
/// Values exclude application callee names. Applications are ordered by their
/// callee positions, including nested applications. Counts bound logical work and
/// retained records; they are not exact allocator-byte or runtime-cost estimates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleExpressionInventory {
    values: Vec<ModuleScalarValue>,
    applications: Vec<ModuleExpressionApplication>,
    work_nodes: usize,
    argument_count: usize,
}

impl ModuleExpressionInventory {
    /// Returns scalar atoms in source order, excluding application callee names.
    #[must_use]
    pub fn values(&self) -> &[ModuleScalarValue] {
        &self.values
    }

    /// Returns applications in callee source order, not inferred execution order.
    #[must_use]
    pub fn applications(&self) -> &[ModuleExpressionApplication] {
        &self.applications
    }

    /// Returns atoms, callees, applications, groups and unary/binary operator nodes.
    #[must_use]
    pub const fn work_nodes(&self) -> usize {
        self.work_nodes
    }

    /// Returns all positional arguments, including those of nested applications.
    #[must_use]
    pub const fn argument_count(&self) -> usize {
        self.argument_count
    }
}

/// One source-written simple-name application within an expression or call statement.
///
/// An application is not a proven procedure call or array access. Argument ranges
/// preserve expression syntax without asserting parameter or target identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleExpressionApplication {
    application: Range<usize>,
    callee: Range<usize>,
    arguments: Range<usize>,
    argument_expressions: Vec<Range<usize>>,
}

impl ModuleExpressionApplication {
    /// Returns the exact callee-through-last-argument source span.
    /// Parenthesized applications include their closing parenthesis.
    #[must_use]
    pub fn application(&self) -> Range<usize> {
        self.application.clone()
    }

    /// Returns the exact callee name, retaining authored brackets.
    #[must_use]
    pub fn callee(&self) -> Range<usize> {
        self.callee.clone()
    }

    /// Returns the authored argument bytes, including parentheses when written.
    /// An implicit statement's outer application has an unparenthesized list.
    #[must_use]
    pub fn arguments(&self) -> Range<usize> {
        self.arguments.clone()
    }

    /// Returns complete positional expression spans, excluding surrounding trivia.
    #[must_use]
    pub fn argument_expressions(&self) -> &[Range<usize>] {
        &self.argument_expressions
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operator {
    Power,
    Positive,
    Negative,
    Multiply,
    Divide,
    IntegerDivide,
    Modulo,
    Add,
    Subtract,
    Concatenate,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Is,
    Like,
    Not,
    And,
    Or,
    Xor,
    Eqv,
    Imp,
}

impl Operator {
    const fn precedence(self) -> u8 {
        match self {
            Self::Power => 14,
            Self::Positive | Self::Negative => 13,
            Self::Multiply | Self::Divide => 12,
            Self::IntegerDivide => 11,
            Self::Modulo => 10,
            Self::Add | Self::Subtract => 9,
            Self::Concatenate => 8,
            Self::Equal
            | Self::NotEqual
            | Self::Less
            | Self::LessEqual
            | Self::Greater
            | Self::GreaterEqual
            | Self::Is
            | Self::Like => 7,
            Self::Not => 6,
            Self::And => 5,
            Self::Or => 4,
            Self::Xor => 3,
            Self::Eqv => 2,
            Self::Imp => 1,
        }
    }

    const fn unary(self) -> bool {
        matches!(self, Self::Positive | Self::Negative | Self::Not)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Atom(usize),
    Callee,
    Unary(Operator, usize),
    Binary(Operator, usize, usize),
    Group(usize),
    Application(usize),
    Pending,
}

#[derive(Debug)]
struct Node {
    kind: NodeKind,
    span: Range<usize>,
}

#[derive(Debug)]
struct PendingOperator {
    operator: Operator,
    start: usize,
}

#[derive(Debug, Clone, Copy)]
enum FrameKind {
    Root,
    ImplicitApplication { node: usize, application: usize },
    Group { node: usize },
    Application { node: usize, application: usize },
}

#[derive(Debug)]
struct Frame {
    kind: FrameKind,
    value_base: usize,
    operator_base: usize,
    has_argument: bool,
}

#[derive(Clone, Copy)]
enum Ceiling {
    Nodes,
    Arguments,
}

impl Ceiling {
    fn error<E>(self) -> ModuleError<E> {
        match self {
            Self::Nodes => ModuleError::ExpressionLimit,
            Self::Arguments => ModuleError::ArgumentLimit,
        }
    }
}

fn reserve<T, E>(
    values: &mut Vec<T>,
    maximum: usize,
    ceiling: Ceiling,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if values.len() >= maximum {
        return Err(ceiling.error());
    }
    if values.len() == values.capacity() {
        let required = values.len().checked_add(1).ok_or_else(|| ceiling.error())?;
        let capacity = values
            .capacity()
            .saturating_mul(2)
            .max(4)
            .min(maximum)
            .max(required);
        reserve_exact(values, capacity - values.len(), check)?;
    }
    Ok(())
}

fn reserve_exact<T, E>(
    values: &mut Vec<T>,
    additional: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    values
        .try_reserve_exact(additional)
        .map_err(|_| ModuleError::AllocationFailed)?;
    check().map_err(ModuleError::Cancelled)
}

struct Parser<'a> {
    source: &'a [u8],
    cursor: usize,
    maximum_nodes: usize,
    maximum_arguments: usize,
    nodes: Vec<Node>,
    values: Vec<ModuleScalarValue>,
    applications: Vec<ModuleExpressionApplication>,
    operands: Vec<usize>,
    operators: Vec<PendingOperator>,
    frames: Vec<Frame>,
    argument_count: usize,
    explicit_callee: Option<Range<usize>>,
    implicit_callee: Option<Range<usize>>,
    implicit_group_assignment: bool,
}

struct Parsed {
    inventory: ModuleExpressionInventory,
    span: Range<usize>,
    root_kind: NodeKind,
    implicit_single_group: bool,
    implicit_group_assignment: bool,
    #[cfg(test)]
    nodes: Vec<Node>,
    #[cfg(test)]
    root: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a [u8], start: usize, maximum_nodes: usize, maximum_arguments: usize) -> Self {
        Self {
            source,
            cursor: start,
            maximum_nodes,
            maximum_arguments,
            nodes: Vec::new(),
            values: Vec::new(),
            applications: Vec::new(),
            operands: Vec::new(),
            operators: Vec::new(),
            frames: Vec::new(),
            argument_count: 0,
            explicit_callee: None,
            implicit_callee: None,
            implicit_group_assignment: false,
        }
    }

    fn node<E>(
        &mut self,
        kind: NodeKind,
        span: Range<usize>,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<usize, ModuleError<E>> {
        reserve(&mut self.nodes, self.maximum_nodes, Ceiling::Nodes, check)?;
        let index = self.nodes.len();
        self.nodes.push(Node { kind, span });
        Ok(index)
    }

    fn operand<E>(
        &mut self,
        node: usize,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), ModuleError<E>> {
        reserve(
            &mut self.operands,
            self.maximum_nodes,
            Ceiling::Nodes,
            check,
        )?;
        self.operands.push(node);
        Ok(())
    }

    fn trivia<E>(
        &mut self,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), ModuleError<E>> {
        self.cursor +=
            logical_whitespace_len(self.source.get(self.cursor..).unwrap_or_default(), check)
                .map_err(ModuleError::Cancelled)?;
        Ok(())
    }

    fn reduce<E>(
        &mut self,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<bool, ModuleError<E>> {
        check().map_err(ModuleError::Cancelled)?;
        let Some(pending) = self.operators.pop() else {
            return Ok(false);
        };
        let base = self.frames.last().map_or(0, |frame| frame.value_base);
        if self.operands.len() <= base {
            return Ok(false);
        }
        let Some(right) = self.operands.pop() else {
            return Ok(false);
        };
        let Some(right_node) = self.nodes.get(right) else {
            return Ok(false);
        };
        let right_end = right_node.span.end;
        let (kind, start) = if pending.operator.unary() {
            (NodeKind::Unary(pending.operator, right), pending.start)
        } else {
            if self.operands.len() <= base {
                return Ok(false);
            }
            let Some(left) = self.operands.pop() else {
                return Ok(false);
            };
            let Some(left_node) = self.nodes.get(left) else {
                return Ok(false);
            };
            (
                NodeKind::Binary(pending.operator, left, right),
                left_node.span.start,
            )
        };
        let node = self.node(kind, start..right_end, check)?;
        self.operand(node, check)?;
        Ok(true)
    }

    fn finish_segment<E>(
        &mut self,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Option<usize>, ModuleError<E>> {
        let Some(frame) = self.frames.last() else {
            return Ok(None);
        };
        let operator_base = frame.operator_base;
        let value_base = frame.value_base;
        while self.operators.len() > operator_base {
            if !self.reduce(check)? {
                return Ok(None);
            }
        }
        if self.operands.len() != value_base.saturating_add(1) {
            return Ok(None);
        }
        Ok(self.operands.pop())
    }

    fn argument<E>(
        &mut self,
        application: usize,
        node: usize,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), ModuleError<E>> {
        check().map_err(ModuleError::Cancelled)?;
        if self.argument_count >= self.maximum_arguments {
            return Err(ModuleError::ArgumentLimit);
        }
        let span = self
            .nodes
            .get(node)
            .map(|node| node.span.clone())
            .ok_or_else(|| ModuleError::Unsupported {
                range: 0..self.source.len(),
            })?;
        if span.start >= span.end || self.source.get(span.clone()).is_none() {
            return Err(ModuleError::Unsupported {
                range: 0..self.source.len(),
            });
        }
        let record =
            self.applications
                .get_mut(application)
                .ok_or_else(|| ModuleError::Unsupported {
                    range: 0..self.source.len(),
                })?;
        reserve(
            &mut record.argument_expressions,
            self.maximum_arguments,
            Ceiling::Arguments,
            check,
        )?;
        record.argument_expressions.push(span);
        self.argument_count = self
            .argument_count
            .checked_add(1)
            .ok_or(ModuleError::ArgumentLimit)?;
        Ok(())
    }

    fn parse<E>(
        mut self,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Option<Parsed>, ModuleError<E>> {
        check().map_err(ModuleError::Cancelled)?;
        // One root frame is fixed overhead. All nested frames have an already
        // charged group/application node, so depth cannot bypass the node ceiling.
        reserve_exact(&mut self.frames, 1, check)?;
        let root_kind = if let Some(callee) = self.implicit_callee.take() {
            self.node(NodeKind::Callee, callee.clone(), check)?;
            let node = self.node(NodeKind::Pending, callee.clone(), check)?;
            reserve(
                &mut self.applications,
                self.maximum_nodes,
                Ceiling::Nodes,
                check,
            )?;
            self.applications.push(ModuleExpressionApplication {
                application: callee.clone(),
                callee,
                arguments: self.cursor..self.cursor,
                argument_expressions: Vec::new(),
            });
            FrameKind::ImplicitApplication {
                node,
                application: 0,
            }
        } else {
            FrameKind::Root
        };
        self.frames.push(Frame {
            kind: root_kind,
            value_base: 0,
            operator_base: 0,
            has_argument: false,
        });
        let mut need_operand = true;
        loop {
            check().map_err(ModuleError::Cancelled)?;
            self.trivia(check)?;
            let token = self.source.get(self.cursor).copied();
            if need_operand {
                if token == Some(b')') {
                    let Some(frame) = self.frames.last() else {
                        return Ok(None);
                    };
                    if let FrameKind::Application { node, application } = frame.kind
                        && !frame.has_argument
                        && self.operands.len() == frame.value_base
                        && self.operators.len() == frame.operator_base
                    {
                        self.close(node, Some(application), check)?;
                        need_operand = false;
                        continue;
                    }
                    return Ok(None);
                }
                if let Some((operator, length)) = unary(self.source, self.cursor) {
                    // Prefix operators do not reduce pending power: both -2 ^ 2
                    // and 2 ^ -2 retain their authored precedence without recursion.
                    reserve(
                        &mut self.operators,
                        self.maximum_nodes,
                        Ceiling::Nodes,
                        check,
                    )?;
                    self.operators.push(PendingOperator {
                        operator,
                        start: self.cursor,
                    });
                    self.cursor += length;
                    continue;
                }
                if token == Some(b'(') {
                    let node = self.node(NodeKind::Pending, self.cursor..self.cursor + 1, check)?;
                    self.cursor += 1;
                    reserve(
                        &mut self.frames,
                        self.maximum_nodes.saturating_add(1),
                        Ceiling::Nodes,
                        check,
                    )?;
                    self.frames.push(Frame {
                        kind: FrameKind::Group { node },
                        value_base: self.operands.len(),
                        operator_base: self.operators.len(),
                        has_argument: false,
                    });
                    continue;
                }
                let Some(value) = atomic_value(self.source, &mut self.cursor, true, check)
                    .map_err(ModuleError::Cancelled)?
                else {
                    return Ok(None);
                };
                // MS-VBAL 3.3.2/3.3.5.3 attach suffixes to numeric literals and
                // ordinary identifiers. The numeric scanner retains one authored
                // suffix; a remaining unsupported suffix is not a binary operator.
                let adjacent_suffix = match value.kind() {
                    ModuleScalarValueKind::UnsignedDecimal
                    | ModuleScalarValueKind::UnsignedHexadecimal
                    | ModuleScalarValueKind::UnsignedOctal => matches!(
                        self.source.get(self.cursor),
                        Some(b'%' | b'&' | b'^' | b'!' | b'#' | b'@')
                    ),
                    ModuleScalarValueKind::UnsignedFloat => matches!(
                        self.source.get(self.cursor),
                        Some(b'%' | b'!' | b'#' | b'@' | b'$')
                    ),
                    ModuleScalarValueKind::Name
                        if self.source.get(value.span().start) != Some(&b'[') =>
                    {
                        matches!(
                            self.source.get(self.cursor),
                            Some(b'%' | b'&' | b'^' | b'!' | b'#' | b'@' | b'$')
                        )
                    }
                    ModuleScalarValueKind::Name
                    | ModuleScalarValueKind::String
                    | ModuleScalarValueKind::Boolean => false,
                };
                if adjacent_suffix {
                    return Ok(None);
                }
                self.trivia(check)?;
                if value.kind() == ModuleScalarValueKind::Name
                    && self.source.get(self.cursor) == Some(&b'(')
                {
                    if self
                        .source
                        .get(value.span())
                        .is_some_and(|name| name.eq_ignore_ascii_case(b"Line"))
                        && self.explicit_callee.as_ref() != Some(&value.span())
                    {
                        return Ok(None);
                    }
                    self.node(NodeKind::Callee, value.span(), check)?;
                    let node = self.node(NodeKind::Pending, value.span(), check)?;
                    reserve(
                        &mut self.applications,
                        self.maximum_nodes,
                        Ceiling::Nodes,
                        check,
                    )?;
                    let application = self.applications.len();
                    self.applications.push(ModuleExpressionApplication {
                        application: value.span(),
                        callee: value.span(),
                        arguments: self.cursor..self.cursor + 1,
                        argument_expressions: Vec::new(),
                    });
                    self.cursor += 1;
                    reserve(
                        &mut self.frames,
                        self.maximum_nodes.saturating_add(1),
                        Ceiling::Nodes,
                        check,
                    )?;
                    self.frames.push(Frame {
                        kind: FrameKind::Application { node, application },
                        value_base: self.operands.len(),
                        operator_base: self.operators.len(),
                        has_argument: false,
                    });
                    continue;
                }
                reserve(&mut self.values, self.maximum_nodes, Ceiling::Nodes, check)?;
                let span = value.span();
                let atom = self.values.len();
                self.values.push(value);
                let node = self.node(NodeKind::Atom(atom), span, check)?;
                self.operand(node, check)?;
                need_operand = false;
                continue;
            }
            if token == Some(b',') {
                let Some(frame) = self.frames.last() else {
                    return Ok(None);
                };
                let application = match frame.kind {
                    FrameKind::Application { application, .. }
                    | FrameKind::ImplicitApplication { application, .. } => application,
                    FrameKind::Root | FrameKind::Group { .. } => return Ok(None),
                };
                let Some(node) = self.finish_segment(check)? else {
                    return Ok(None);
                };
                self.argument(application, node, check)?;
                if let Some(frame) = self.frames.last_mut() {
                    frame.has_argument = true;
                }
                self.cursor += 1;
                need_operand = true;
                continue;
            }
            if token == Some(b')') {
                let Some(frame) = self.frames.last() else {
                    return Ok(None);
                };
                let kind = frame.kind;
                let Some(child) = self.finish_segment(check)? else {
                    return Ok(None);
                };
                match kind {
                    FrameKind::Root | FrameKind::ImplicitApplication { .. } => return Ok(None),
                    FrameKind::Group { node } => {
                        let record =
                            self.nodes
                                .get_mut(node)
                                .ok_or_else(|| ModuleError::Unsupported {
                                    range: 0..self.source.len(),
                                })?;
                        record.kind = NodeKind::Group(child);
                        self.close(node, None, check)?;
                    }
                    FrameKind::Application { node, application } => {
                        self.argument(application, child, check)?;
                        self.close(node, Some(application), check)?;
                    }
                }
                continue;
            }
            if token.is_none() {
                if self.frames.len() != 1 {
                    return Ok(None);
                }
                let Some(mut root) = self.finish_segment(check)? else {
                    return Ok(None);
                };
                let mut implicit_single_group = false;
                if let Some(Frame {
                    kind: FrameKind::ImplicitApplication { node, application },
                    ..
                }) = self.frames.last()
                {
                    let node = *node;
                    let application = *application;
                    implicit_single_group = self
                        .applications
                        .get(application)
                        .is_some_and(|record| record.argument_expressions.is_empty())
                        && self
                            .nodes
                            .get(root)
                            .is_some_and(|record| matches!(record.kind, NodeKind::Group(_)));
                    self.argument(application, root, check)?;
                    let end = self.nodes[root].span.end;
                    self.nodes[node].kind = NodeKind::Application(application);
                    self.nodes[node].span.end = end;
                    self.applications[application].application.end = end;
                    self.applications[application].arguments.end = end;
                    root = node;
                }
                let Some(root_node) = self.nodes.get(root) else {
                    return Ok(None);
                };
                let span = root_node.span.clone();
                let root_kind = root_node.kind;
                // Verify flat indices without walking a recursive tree. Pending
                // placeholders must never reach an admitted source inventory.
                for node in &self.nodes {
                    check().map_err(ModuleError::Cancelled)?;
                    let valid = match node.kind {
                        NodeKind::Atom(index) => index < self.values.len(),
                        NodeKind::Callee => true,
                        NodeKind::Unary(operator, child) => {
                            operator.unary() && child < self.nodes.len()
                        }
                        NodeKind::Binary(operator, left, right) => {
                            !operator.unary() && left < self.nodes.len() && right < self.nodes.len()
                        }
                        NodeKind::Group(child) => child < self.nodes.len(),
                        NodeKind::Application(index) => index < self.applications.len(),
                        NodeKind::Pending => false,
                    };
                    if !valid {
                        return Ok(None);
                    }
                }
                let inventory = ModuleExpressionInventory {
                    values: self.values,
                    applications: self.applications,
                    work_nodes: self.nodes.len(),
                    argument_count: self.argument_count,
                };
                return Ok(Some(Parsed {
                    inventory,
                    span,
                    root_kind,
                    implicit_single_group,
                    implicit_group_assignment: self.implicit_group_assignment,
                    #[cfg(test)]
                    nodes: self.nodes,
                    #[cfg(test)]
                    root,
                }));
            }
            let Some((operator, length)) = binary(self.source, self.cursor) else {
                return Ok(None);
            };
            let operator_base = self.frames.last().map_or(0, |frame| frame.operator_base);
            // MS-VBAL 5.6.9.1 defines every binary operator, including power,
            // as left-associative. Unary precedence is handled independently.
            // https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/24cb214a-ef26-4e36-973d-43f715c8b127
            while self.operators.len() > operator_base
                && self
                    .operators
                    .last()
                    .is_some_and(|pending| pending.operator.precedence() >= operator.precedence())
            {
                if !self.reduce(check)? {
                    return Ok(None);
                }
            }
            // `Target (index) = value` has an indexed-assignment destination,
            // not a proven implicit call with a comparison argument. Inspect
            // the reduced left operand before later RHS operators hide it.
            if operator == Operator::Equal
                && let Some(Frame {
                    kind: FrameKind::ImplicitApplication { application, .. },
                    ..
                }) = self.frames.last()
                && let Some(outer) = self.applications.get(*application)
                && outer.argument_expressions.is_empty()
                && self
                    .operands
                    .last()
                    .and_then(|index| self.nodes.get(*index))
                    .is_some_and(|left| {
                        matches!(left.kind, NodeKind::Group(_))
                            && left.span.start == outer.arguments.start
                    })
            {
                self.implicit_group_assignment = true;
            }
            reserve(
                &mut self.operators,
                self.maximum_nodes,
                Ceiling::Nodes,
                check,
            )?;
            self.operators.push(PendingOperator {
                operator,
                start: self.cursor,
            });
            self.cursor += length;
            need_operand = true;
        }
    }

    fn close<E>(
        &mut self,
        node: usize,
        application: Option<usize>,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), ModuleError<E>> {
        check().map_err(ModuleError::Cancelled)?;
        if self.source.get(self.cursor) != Some(&b')') || self.frames.len() <= 1 {
            return Err(ModuleError::Unsupported {
                range: 0..self.source.len(),
            });
        }
        if self.nodes.get(node).is_none()
            || application.is_some_and(|index| self.applications.get(index).is_none())
        {
            return Err(ModuleError::Unsupported {
                range: 0..self.source.len(),
            });
        }
        self.cursor += 1;
        self.frames.pop();
        let record = self
            .nodes
            .get_mut(node)
            .ok_or_else(|| ModuleError::Unsupported {
                range: 0..self.source.len(),
            })?;
        record.span.end = self.cursor;
        if let Some(application) = application {
            record.kind = NodeKind::Application(application);
        }
        if let Some(index) = application {
            let application =
                self.applications
                    .get_mut(index)
                    .ok_or_else(|| ModuleError::Unsupported {
                        range: 0..self.source.len(),
                    })?;
            application.application.end = self.cursor;
            application.arguments.end = self.cursor;
        }
        self.operand(node, check)
    }
}

fn keyword(source: &[u8], cursor: usize, spelling: &[u8]) -> bool {
    let Some(end) = cursor.checked_add(spelling.len()) else {
        return false;
    };
    source
        .get(cursor..end)
        .is_some_and(|token| token.eq_ignore_ascii_case(spelling))
        && !source
            .get(end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn unary(source: &[u8], cursor: usize) -> Option<(Operator, usize)> {
    match source.get(cursor) {
        Some(b'+') => Some((Operator::Positive, 1)),
        Some(b'-') => Some((Operator::Negative, 1)),
        _ if keyword(source, cursor, b"Not") => Some((Operator::Not, 3)),
        _ => None,
    }
}

fn binary(source: &[u8], cursor: usize) -> Option<(Operator, usize)> {
    let bytes = source.get(cursor..)?;
    for (spelling, operator) in [
        (b"<>".as_slice(), Operator::NotEqual),
        (b"<=".as_slice(), Operator::LessEqual),
        (b">=".as_slice(), Operator::GreaterEqual),
    ] {
        if bytes.starts_with(spelling) {
            return Some((operator, 2));
        }
    }
    let symbolic = match bytes.first()? {
        b'^' => Some(Operator::Power),
        b'*' => Some(Operator::Multiply),
        b'/' => Some(Operator::Divide),
        b'\\' => Some(Operator::IntegerDivide),
        b'+' => Some(Operator::Add),
        b'-' => Some(Operator::Subtract),
        // Ambiguous operand-following radix tokens remain on the native route;
        // a distinct ampersand can still precede a decimal or radix operand.
        b'&' if !radix_integer_prefix(bytes) => Some(Operator::Concatenate),
        b'=' => Some(Operator::Equal),
        b'<' => Some(Operator::Less),
        b'>' => Some(Operator::Greater),
        _ => None,
    };
    if let Some(operator) = symbolic {
        return Some((operator, 1));
    }
    for (spelling, operator) in [
        (b"Mod".as_slice(), Operator::Modulo),
        (b"Is".as_slice(), Operator::Is),
        (b"Like".as_slice(), Operator::Like),
        (b"And".as_slice(), Operator::And),
        (b"Or".as_slice(), Operator::Or),
        (b"Xor".as_slice(), Operator::Xor),
        (b"Eqv".as_slice(), Operator::Eqv),
        (b"Imp".as_slice(), Operator::Imp),
    ] {
        if keyword(source, cursor, spelling) {
            return Some((operator, spelling.len()));
        }
    }
    None
}

pub(super) fn inventory<E>(
    source: &[u8],
    start: usize,
    source_range: &Range<usize>,
    maximum_nodes: usize,
    maximum_arguments: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<(Range<usize>, ModuleExpressionInventory)>, ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if source_range.end.checked_sub(source_range.start) != Some(source.len())
        || start > source.len()
    {
        return Ok(None);
    }
    let Some(parsed) = Parser::new(source, start, maximum_nodes, maximum_arguments).parse(check)?
    else {
        return Ok(None);
    };
    rebase_inventory(parsed, source_range, check).map(Some)
}

pub(super) fn value_application_statement<E>(
    source: &[u8],
    callee: Range<usize>,
    source_range: Range<usize>,
    maximum_nodes: usize,
    maximum_arguments: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleValueApplication>, ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if source_range.end.checked_sub(source_range.start) != Some(source.len())
        || callee.start != 0
        || callee.end >= source.len()
    {
        return Ok(None);
    }
    let mut cursor = 0;
    if super::module_body::simple_name(source, &mut cursor, check)
        .map_err(ModuleError::Cancelled)?
        != Some(callee.clone())
    {
        return Ok(None);
    }
    cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)
        .map_err(ModuleError::Cancelled)?;
    if source.get(cursor) != Some(&b'(')
        || !super::module_body::implicit_call_tail_is_complete(source, check)
            .map_err(ModuleError::Cancelled)?
    {
        return Ok(None);
    }
    let Some(parsed) = Parser::new(source, 0, maximum_nodes, maximum_arguments).parse(check)?
    else {
        return Ok(None);
    };
    if parsed.root_kind != NodeKind::Application(0)
        || parsed.inventory.applications.first().is_none_or(|outer| {
            outer.callee != callee
                || outer.application != parsed.span
                || outer.arguments.start != cursor
                || outer.arguments.end != parsed.span.end
                || outer
                    .arguments
                    .end
                    .checked_sub(1)
                    .and_then(|end| source.get(end))
                    != Some(&b')')
        })
    {
        return Ok(None);
    }
    let (application, inventory) = rebase_inventory(parsed, &source_range, check)?;
    let Some(outer) = inventory.applications.first() else {
        return Ok(None);
    };
    let callee = outer.callee();
    let arguments = outer.arguments();
    Ok(Some(ModuleValueApplication {
        statement: source_range,
        application,
        callee,
        arguments,
        inventory,
    }))
}

pub(super) fn implicit_expression_call<E>(
    source: &[u8],
    callee: Range<usize>,
    arguments_start: usize,
    source_range: Range<usize>,
    maximum_nodes: usize,
    maximum_arguments: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleExpressionCall>, ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if source_range.end.checked_sub(source_range.start) != Some(source.len())
        || callee.start != 0
        || callee.end >= arguments_start
        || arguments_start >= source.len()
    {
        return Ok(None);
    }
    let mut cursor = 0;
    if super::module_body::simple_name(source, &mut cursor, check)
        .map_err(ModuleError::Cancelled)?
        != Some(callee.clone())
    {
        return Ok(None);
    }
    cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)
        .map_err(ModuleError::Cancelled)?;
    if cursor != arguments_start {
        return Ok(None);
    }
    if !super::module_body::implicit_call_tail_is_complete(source, check)
        .map_err(ModuleError::Cancelled)?
    {
        return Ok(None);
    }
    let mut parser = Parser::new(source, arguments_start, maximum_nodes, maximum_arguments);
    parser.implicit_callee = Some(callee.clone());
    let Some(parsed) = parser.parse(check)? else {
        return Ok(None);
    };
    // The outer frame is a source-written statement application, not fabricated
    // parentheses. Its comma boundaries share the nested expression machinery.
    if parsed.root_kind != NodeKind::Application(0)
        || parsed.inventory.applications.first().is_none_or(|outer| {
            outer.callee != callee
                || outer.application != parsed.span
                || outer.arguments.start != arguments_start
                || outer.arguments.end != parsed.span.end
                || outer.argument_expressions.is_empty()
        })
    {
        return Ok(None);
    }
    // A complete grouped argument has a native value-application callee role;
    // a leading group followed by '=' instead needs indexed-write evidence.
    // Other grouped operator operands remain statement-call arguments.
    if parsed.implicit_single_group || parsed.implicit_group_assignment {
        return Ok(None);
    }
    let (_, inventory) = rebase_inventory(parsed, &source_range, check)?;
    let Some(outer) = inventory.applications.first() else {
        return Ok(None);
    };
    let callee = outer.callee();
    let arguments = outer.arguments();
    Ok(Some(ModuleExpressionCall {
        statement: source_range,
        callee,
        arguments,
        inventory,
    }))
}

pub(super) fn expression_call<E>(
    source: &[u8],
    callee: Range<usize>,
    source_range: Range<usize>,
    maximum_nodes: usize,
    maximum_arguments: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleExpressionCall>, ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if source_range.end.checked_sub(source_range.start) != Some(source.len())
        || callee.start >= callee.end
        || source.get(callee.clone()).is_none()
    {
        return Ok(None);
    }
    // The parser starts at a supplied byte offset, not a lexer token boundary.
    // A suffix of an identifier can otherwise look like a new complete callee.
    if callee
        .start
        .checked_sub(1)
        .and_then(|previous| source.get(previous))
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        return Ok(None);
    }
    let mut parser = Parser::new(source, callee.start, maximum_nodes, maximum_arguments);
    // Explicit Call permits the contextual outer Line name; nested applications
    // still require the existing unambiguous value-application spelling.
    parser.explicit_callee = Some(callee.clone());
    let Some(parsed) = parser.parse(check)? else {
        return Ok(None);
    };
    check().map_err(ModuleError::Cancelled)?;
    let Some(outer) = parsed.inventory.applications.first() else {
        return Ok(None);
    };
    // A first source record alone is insufficient: groups, unary/binary roots
    // and trailing applications must not masquerade as an explicit Call list.
    if parsed.root_kind != NodeKind::Application(0)
        || parsed.span != outer.application
        || parsed.span.start != callee.start
        || outer.callee != callee
        || outer.arguments.start < callee.end
        || outer.arguments.end != parsed.span.end
        || source.get(outer.arguments.start) != Some(&b'(')
        || outer
            .arguments
            .end
            .checked_sub(1)
            .and_then(|end| source.get(end))
            != Some(&b')')
    {
        return Ok(None);
    }
    let (_, inventory) = rebase_inventory(parsed, &source_range, check)?;
    let Some(outer) = inventory.applications.first() else {
        return Ok(None);
    };
    let callee = outer.callee();
    let arguments = outer.arguments();
    Ok(Some(ModuleExpressionCall {
        statement: source_range,
        callee,
        arguments,
        inventory,
    }))
}

fn rebase_inventory<E>(
    mut parsed: Parsed,
    source_range: &Range<usize>,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(Range<usize>, ModuleExpressionInventory), ModuleError<E>> {
    let rebase = |range: &mut Range<usize>| {
        range.start += source_range.start;
        range.end += source_range.start;
    };
    for value in &mut parsed.inventory.values {
        check().map_err(ModuleError::Cancelled)?;
        value.rebase(source_range);
    }
    for application in &mut parsed.inventory.applications {
        check().map_err(ModuleError::Cancelled)?;
        rebase(&mut application.application);
        rebase(&mut application.callee);
        rebase(&mut application.arguments);
        for argument in &mut application.argument_expressions {
            check().map_err(ModuleError::Cancelled)?;
            rebase(argument);
        }
    }
    check().map_err(ModuleError::Cancelled)?;
    rebase(&mut parsed.span);
    Ok((parsed.span, parsed.inventory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModuleFloatSuffix, ModuleIntegerSuffix};

    fn parse(source: &str) -> Parsed {
        Parser::new(source.as_bytes(), 0, source.len(), source.len())
            .parse(&mut || Ok::<(), ()>(()))
            .unwrap()
            .expect("complete expression")
    }

    #[test]
    fn value_application_private_reader_requires_exact_head_root_and_source_boundaries() {
        let source = b"Target\t (value, Inner(other)) ";
        let read = |callee, range| {
            value_application_statement(
                source,
                callee,
                range,
                source.len(),
                source.len(),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
        };
        let application = read(0..6, 42..42 + source.len()).unwrap();
        assert_eq!(application.statement(), 42..42 + source.len());
        assert_eq!(application.callee(), 42..48);
        assert_eq!(application.application(), 42..41 + source.len());
        assert_eq!(application.arguments(), 50..41 + source.len());
        assert_eq!(application.inventory().applications().len(), 2);
        assert_eq!(application.inventory().values().len(), 2);
        assert_eq!(application.inventory().work_nodes(), 6);
        assert_eq!(application.inventory().argument_count(), 3);
        for callee in [0..0, 0..5, 0..7, 1..6, 0..source.len() + 1] {
            assert!(
                read(callee.clone(), 0..source.len()).is_none(),
                "{callee:?}"
            );
        }
        for range in [
            0..source.len() - 1,
            0..source.len() + 1,
            usize::MAX..usize::MAX,
        ] {
            assert!(read(0..6, range.clone()).is_none(), "{range:?}");
        }
        for source in [
            b"Target value".as_slice(),
            b"Target (value), other",
            b"Target (value) * -2",
            b"Target (value) + Inner(other)",
            b"Target (value) = other And True",
            b"Target (value) trailing",
            b"Target (value).member",
            b"Target (value)(other)",
            b"Target (value) _\n",
            b"Target (value) _\r\n\t ",
        ] {
            assert!(
                value_application_statement(
                    source,
                    0..6,
                    0..source.len(),
                    source.len(),
                    source.len(),
                    &mut || Ok::<(), ()>(()),
                )
                .unwrap()
                .is_none(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn implicit_call_private_boundaries_require_complete_head_gap_and_source_identity() {
        let source = b"Target  value+1,Inner(other)*-2";
        let read = |callee, arguments_start, range| {
            implicit_expression_call(
                source,
                callee,
                arguments_start,
                range,
                source.len(),
                source.len(),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
        };
        let call = read(0..6, 8, 42..42 + source.len()).unwrap();
        assert_eq!(call.statement(), 42..42 + source.len());
        assert_eq!(call.callee(), 42..48);
        assert_eq!(call.arguments(), 50..42 + source.len());
        assert_eq!(
            call.inventory().applications()[0].application(),
            call.statement()
        );
        for callee in [0..0, 0..5, 0..7, 1..6, 0..source.len() + 1] {
            assert!(read(callee, 8, 0..source.len()).is_none());
        }
        for start in [0, 6, 7, 9, source.len(), usize::MAX] {
            assert!(
                read(0..6, start, 0..source.len()).is_none(),
                "argument boundary {start}"
            );
        }
        for range in [
            0..source.len() - 1,
            0..source.len() + 1,
            usize::MAX..usize::MAX,
        ] {
            assert!(read(0..6, 8, range).is_none());
        }
        for source in [b"Target value _\n".as_slice(), b"Target value _\r\n\t "] {
            assert!(
                implicit_expression_call(
                    source,
                    0..6,
                    7,
                    0..source.len(),
                    source.len(),
                    source.len(),
                    &mut || Ok::<(), ()>(())
                )
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn implicit_outer_application_and_deep_nested_arguments_share_iterative_ceilings() {
        let depth = 1024;
        let text = format!(
            "Target {}value{}",
            "Inner(".repeat(depth),
            ")".repeat(depth)
        );
        let nodes = depth * 2 + 3;
        let arguments = depth + 1;
        let call = implicit_expression_call(
            text.as_bytes(),
            0..6,
            7,
            0..text.len(),
            nodes,
            arguments,
            &mut || Ok::<(), ()>(()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(call.inventory().work_nodes(), nodes);
        assert_eq!(call.inventory().argument_count(), arguments);
        assert_eq!(call.inventory().applications().len(), depth + 1);
        let argument = 7..text.len();
        assert_eq!(call.argument_expressions(), std::slice::from_ref(&argument));
        assert_eq!(
            implicit_expression_call(
                text.as_bytes(),
                0..6,
                7,
                0..text.len(),
                nodes - 1,
                arguments,
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::ExpressionLimit)
        );
        assert_eq!(
            implicit_expression_call(
                text.as_bytes(),
                0..6,
                7,
                0..text.len(),
                nodes,
                arguments - 1,
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::ArgumentLimit)
        );
        drop(call);
    }

    #[test]
    fn implicit_single_leading_groups_cannot_claim_a_statement_application_role() {
        for tail in [
            "(value)",
            "((value))",
            "(Inner(value, other))",
            "(\"a,b\")",
            "(value + 1)",
        ] {
            for newline in ["\n", "\r\n"] {
                for source in [
                    format!("[Target] {tail}"),
                    format!("[Target] _{newline} {tail}"),
                ] {
                    let arguments_start = source.find(tail).unwrap();
                    assert!(
                        implicit_expression_call(
                            source.as_bytes(),
                            0..8,
                            arguments_start,
                            42..42 + source.len(),
                            source.len(),
                            source.len(),
                            &mut || Ok::<(), ()>(()),
                        )
                        .unwrap()
                        .is_none(),
                        "{source:?}"
                    );
                }
            }
        }
        for tail in [
            "(value), other",
            "((value)), Inner(other)",
            "(value) * -2",
            "(value) + Inner(other)",
            "\"(value)\"",
        ] {
            let source = format!("[Target] {tail}");
            assert!(
                implicit_expression_call(
                    source.as_bytes(),
                    0..8,
                    9,
                    42..42 + source.len(),
                    source.len(),
                    source.len(),
                    &mut || Ok::<(), ()>(()),
                )
                .unwrap()
                .is_some(),
                "{source:?}"
            );
        }
        let source = b"Call Target((value))";
        assert!(
            expression_call(
                source,
                5..11,
                42..42 + source.len(),
                source.len(),
                source.len(),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn implicit_grouped_destinations_do_not_become_comparison_call_arguments() {
        for tail in [
            "(value) = other",
            "((value)) = other + 1",
            "(value + 1) = other And True",
            "(Inner(value)) = other = 0",
        ] {
            for newline in ["\n", "\r\n"] {
                let source = format!("[Target] _{newline} {tail}");
                assert!(
                    implicit_expression_call(
                        source.as_bytes(),
                        0..8,
                        source.find(tail).unwrap(),
                        42..42 + source.len(),
                        source.len(),
                        source.len(),
                        &mut || Ok::<(), ()>(()),
                    )
                    .unwrap()
                    .is_none(),
                    "{source:?}"
                );
            }
        }
        for tail in [
            "value = other",
            "(value) + 1 = other",
            "-(value) = other",
            "Not (value) = other",
            "other, (value) = other",
            "value And (other) = value",
            "(value) + Inner(other) = other",
        ] {
            let source = format!("[Target] {tail}");
            let call = implicit_expression_call(
                source.as_bytes(),
                0..8,
                9,
                42..42 + source.len(),
                source.len(),
                source.len(),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .unwrap_or_else(|| panic!("complete comparison call expected: {source}"));
            assert_eq!(call.statement(), 42..42 + source.len());
            assert_eq!(call.callee(), 42..50);
            assert_eq!(call.arguments(), 51..42 + source.len());
        }
        let source = b"Call Target((value) = other)";
        assert!(
            expression_call(
                source,
                5..11,
                42..42 + source.len(),
                source.len(),
                source.len(),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn float_expression_atoms_preserve_exact_rebased_metadata_and_one_node() {
        for atom in [
            ".5", "5.", "1.0", "1E2", "1e+2", "1D-2", "1d2", "1.E+2", ".5d-2", "007",
        ] {
            for (suffix, marker) in [
                (None, ""),
                (Some(ModuleFloatSuffix::Exclamation), "!"),
                (Some(ModuleFloatSuffix::Hash), "#"),
                (Some(ModuleFloatSuffix::At), "@"),
            ] {
                if atom == "007" && suffix.is_none() {
                    continue;
                }
                let spelling = format!("{atom}{marker}");
                let range = 9..9 + spelling.len();
                let (span, inventory) =
                    super::inventory(spelling.as_bytes(), 0, &range, 1, 0, &mut || {
                        Ok::<(), ()>(())
                    })
                    .unwrap()
                    .unwrap();
                assert_eq!(span, range);
                assert_eq!(inventory.work_nodes(), 1);
                assert_eq!(inventory.argument_count(), 0);
                assert!(inventory.applications().is_empty());
                assert_eq!(inventory.values().len(), 1);
                let value = &inventory.values()[0];
                assert_eq!(value.span(), range);
                assert_eq!(value.kind(), ModuleScalarValueKind::UnsignedFloat);
                assert_eq!(value.integer_suffix(), None);
                assert_eq!(value.float_suffix(), suffix);
                let original = format!("prefix___{spelling}tail");
                assert_eq!(&original[value.span()], spelling);
                assert!(matches!(
                    super::inventory(spelling.as_bytes(), 0, &range, 0, 0, &mut || Ok::<(), ()>(
                        ()
                    )),
                    Err(ModuleError::ExpressionLimit)
                ));
            }
        }
    }

    #[test]
    fn float_operator_boundaries_keep_exponent_signs_atomic_without_integer_suffix_rules() {
        for (source, work, spellings) in [
            ("1.5^2", 3, ["1.5", "2"]),
            ("1.5!^2", 3, ["1.5!", "2"]),
            ("1D-2^2", 3, ["1D-2", "2"]),
            ("7#^1.5", 3, ["7#", "1.5"]),
            ("1.5&8", 3, ["1.5", "8"]),
            ("7#&8", 3, ["7#", "8"]),
            ("1d+2&8", 3, ["1d+2", "8"]),
            ("1.5 & &2", 3, ["1.5", "&2"]),
            ("1.5&&2", 3, ["1.5", "&2"]),
            ("1.5e+2+3", 3, ["1.5e+2", "3"]),
            ("1.5!+2", 3, ["1.5!", "2"]),
            ("(1.5)^2.5", 4, ["1.5", "2.5"]),
            ("-1E-2 + +.5D+3#", 5, ["1E-2", ".5D+3#"]),
            ("- _\r\n 1e-2 + _\r\n +.5d+3@", 5, ["1e-2", ".5d+3@"]),
        ] {
            let parsed = parse(source);
            assert_eq!(parsed.span, 0..source.len());
            assert_eq!(parsed.inventory.work_nodes(), work, "{source}");
            assert_eq!(parsed.inventory.argument_count(), 0);
            assert_eq!(parsed.inventory.values().len(), 2);
            for (value, spelling) in parsed.inventory.values().iter().zip(spellings) {
                assert_eq!(&source[value.span()], spelling);
                if value.kind() == ModuleScalarValueKind::UnsignedFloat {
                    assert_eq!(value.integer_suffix(), None);
                }
            }
            for maximum in [0, work - 1] {
                assert!(
                    matches!(
                        Parser::new(source.as_bytes(), 0, maximum, 0)
                            .parse(&mut || Ok::<(), ()>(())),
                        Err(ModuleError::ExpressionLimit)
                    ),
                    "{source}"
                );
            }
        }
        for source in [
            "1E2^",
            "1.5&",
            "1.5&2",
            "7#&7",
            "1D-2&H1",
            "1.5%",
            "1.5$",
            "1.5!#",
            "7#@",
            "1E+True",
            "1.5e- +2",
            "2^3",
            "2&3",
            "value^other",
            "value&other",
        ] {
            assert!(
                Parser::new(source.as_bytes(), 0, source.len(), source.len())
                    .parse(&mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn integer_suffix_expression_atoms_retain_exact_rebased_metadata_without_extra_work() {
        for (atom, kind) in [
            ("001", ModuleScalarValueKind::UnsignedDecimal),
            ("&H1E2", ModuleScalarValueKind::UnsignedHexadecimal),
            ("&hF", ModuleScalarValueKind::UnsignedHexadecimal),
            ("&O7", ModuleScalarValueKind::UnsignedOctal),
            ("&o7", ModuleScalarValueKind::UnsignedOctal),
            ("&77", ModuleScalarValueKind::UnsignedOctal),
        ] {
            for suffix in [
                ModuleIntegerSuffix::Percent,
                ModuleIntegerSuffix::Ampersand,
                ModuleIntegerSuffix::Caret,
            ] {
                let source = format!("{atom}{}", suffix.as_char());
                let range = 9..9 + source.len();
                let (span, inventory) =
                    super::inventory(source.as_bytes(), 0, &range, 1, 0, &mut || Ok::<(), ()>(()))
                        .unwrap()
                        .unwrap();
                assert_eq!(span, range);
                assert_eq!(inventory.work_nodes(), 1);
                assert_eq!(inventory.argument_count(), 0);
                assert!(inventory.applications().is_empty());
                assert_eq!(inventory.values().len(), 1);
                let value = &inventory.values()[0];
                assert_eq!(value.span(), range);
                assert_eq!(value.kind(), kind);
                assert_eq!(value.integer_suffix(), Some(suffix));
                assert_eq!(
                    value.integer_suffix().unwrap().as_char(),
                    source.chars().last().unwrap()
                );
                assert!(matches!(
                    super::inventory(source.as_bytes(), 0, &range, 0, 0, &mut || Ok::<(), ()>(())),
                    Err(ModuleError::ExpressionLimit)
                ));
            }
        }
    }

    #[test]
    fn integer_suffixes_do_not_fabricate_power_or_concat_from_adjacent_token_tails() {
        for source in [
            "1^2",
            "1&2",
            "1%2",
            "&H1^2",
            "&O7&2",
            "&77%2",
            "1^^",
            "1&&",
            "1%%",
            "1%^2",
            "1&^2",
            "1^&2",
            "1%!",
            "1&#",
            "1^@",
            "1%$",
            "&H%",
            "&O&",
            "&^",
            "1%other",
            "1%(2)",
            "value% + 2",
            "[value]&",
            "True^",
            "\"text\"%",
            "1.5%",
        ] {
            assert!(
                Parser::new(source.as_bytes(), 0, source.len(), source.len())
                    .parse(&mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
        for (source, work, expected) in [
            (
                "1% + &H2& * -&o3^",
                6,
                vec![
                    ("1%", Some(ModuleIntegerSuffix::Percent)),
                    ("&H2&", Some(ModuleIntegerSuffix::Ampersand)),
                    ("&o3^", Some(ModuleIntegerSuffix::Caret)),
                ],
            ),
            (
                "1^ ^ 2",
                3,
                vec![("1^", Some(ModuleIntegerSuffix::Caret)), ("2", None)],
            ),
            (
                "1& & &H2%",
                3,
                vec![
                    ("1&", Some(ModuleIntegerSuffix::Ampersand)),
                    ("&H2%", Some(ModuleIntegerSuffix::Percent)),
                ],
            ),
            (
                "(1%)^2",
                4,
                vec![("1%", Some(ModuleIntegerSuffix::Percent)), ("2", None)],
            ),
            (
                "[a] & &77^",
                3,
                vec![("[a]", None), ("&77^", Some(ModuleIntegerSuffix::Caret))],
            ),
        ] {
            let parsed = parse(source);
            assert_eq!(parsed.span, 0..source.len());
            assert_eq!(parsed.inventory.work_nodes(), work);
            assert_eq!(parsed.inventory.values().len(), expected.len());
            for (value, (spelling, suffix)) in parsed.inventory.values().iter().zip(expected) {
                assert_eq!(&source[value.span()], spelling);
                assert_eq!(value.integer_suffix(), suffix);
            }
        }
        for source in ["[a] &77%", "[a] &H1&", "[a] &O7^", "True&77%", "(1%)&77"] {
            assert!(
                Parser::new(source.as_bytes(), 0, source.len(), 0)
                    .parse(&mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn radix_expression_inventory_preserves_exact_rebased_atoms_applications_and_work() {
        for newline in ["\n", "\r\n"] {
            let source = format!("Target(value + _{newline} &H0A, Inner(&77) * - _{newline} &o2)");
            let range = 9..9 + source.len();
            let (span, inventory) = super::inventory(
                source.as_bytes(),
                0,
                &range,
                11,
                3,
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .unwrap();
            assert_eq!(span, range);
            assert_eq!(inventory.work_nodes(), 11);
            assert_eq!(inventory.argument_count(), 3);
            for (value, (spelling, kind)) in inventory.values().iter().zip([
                ("value", ModuleScalarValueKind::Name),
                ("&H0A", ModuleScalarValueKind::UnsignedHexadecimal),
                ("&77", ModuleScalarValueKind::UnsignedOctal),
                ("&o2", ModuleScalarValueKind::UnsignedOctal),
            ]) {
                assert_eq!(value.kind(), kind);
                let relative = value.span().start - range.start..value.span().end - range.start;
                assert_eq!(&source[relative], spelling);
            }
            assert_eq!(inventory.values().len(), 4);
            assert_eq!(inventory.applications().len(), 2);
            let outer = &inventory.applications()[0];
            assert_eq!(outer.application(), range);
            assert_eq!(outer.callee(), 9..15);
            assert_eq!(outer.argument_expressions().len(), 2);
            for (argument, spelling) in outer.argument_expressions().iter().zip([
                format!("value + _{newline} &H0A"),
                format!("Inner(&77) * - _{newline} &o2"),
            ]) {
                assert_eq!(&source[argument.start - 9..argument.end - 9], spelling);
            }
            let nested = &inventory.applications()[1];
            assert_eq!(
                &source[nested.callee().start - 9..nested.callee().end - 9],
                "Inner"
            );
            assert_eq!(nested.argument_expressions().len(), 1);
            let argument = &nested.argument_expressions()[0];
            assert_eq!(&source[argument.start - 9..argument.end - 9], "&77");
            for maximum in [0, 1, 10] {
                assert!(matches!(
                    super::inventory(
                        source.as_bytes(),
                        0,
                        &range,
                        maximum,
                        3,
                        &mut || Ok::<(), ()>(())
                    ),
                    Err(ModuleError::ExpressionLimit)
                ));
            }
            for maximum in [0, 1, 2] {
                assert!(matches!(
                    super::inventory(source.as_bytes(), 0, &range, 11, maximum, &mut || Ok::<
                        (),
                        (),
                    >(
                        ()
                    )),
                    Err(ModuleError::ArgumentLimit)
                ));
            }
        }
        for source in ["&H1+2", "&hF + &O7", "&77 * -&o2", "&H1 ^ 2"] {
            let parsed = parse(source);
            assert_eq!(parsed.span, 0..source.len());
            assert!(parsed.inventory.values().iter().any(|value| matches!(
                value.kind(),
                ModuleScalarValueKind::UnsignedHexadecimal | ModuleScalarValueKind::UnsignedOctal
            )));
        }
    }

    #[test]
    fn ambiguous_operand_following_radix_prefixes_do_not_become_concat_or_name_reads() {
        for left in ["[a]", "True", "\"text\"", "(value)", "Inner()"] {
            for radix in ["&77", "&O7", "&o7", "&H1", "&hA", "&Hfoo"] {
                for separator in ["", " ", " _\r\n "] {
                    let source = format!("{left}{separator}{radix}");
                    assert!(
                        Parser::new(source.as_bytes(), 0, source.len(), 0)
                            .parse(&mut || Ok::<(), ()>(()))
                            .unwrap()
                            .is_none(),
                        "{source}"
                    );
                }
            }
        }
        for (source, right, kind) in [
            ("[a] & 77", "77", ModuleScalarValueKind::UnsignedDecimal),
            ("[a] & &77", "&77", ModuleScalarValueKind::UnsignedOctal),
            ("[a] &&77", "&77", ModuleScalarValueKind::UnsignedOctal),
            (
                "\"x\" & &H1",
                "&H1",
                ModuleScalarValueKind::UnsignedHexadecimal,
            ),
            ("True&&O7", "&O7", ModuleScalarValueKind::UnsignedOctal),
            ("[a] &H", "H", ModuleScalarValueKind::Name),
            ("[a] &O", "O", ModuleScalarValueKind::Name),
            ("[a] &Other", "Other", ModuleScalarValueKind::Name),
            ("[a] &Hmore", "Hmore", ModuleScalarValueKind::Name),
        ] {
            let parsed = parse(source);
            assert_eq!(parsed.span, 0..source.len());
            assert_eq!(parsed.inventory.work_nodes(), 3);
            let NodeKind::Binary(Operator::Concatenate, _, _) = parsed.nodes[parsed.root].kind
            else {
                panic!("a distinct concat token must retain its operator")
            };
            assert_eq!(parsed.inventory.values().len(), 2);
            assert_eq!(parsed.inventory.values()[1].kind(), kind);
            assert_eq!(&source[parsed.inventory.values()[1].span()], right);
        }
    }

    #[test]
    fn radix_prefix_probe_uses_only_complete_numeric_prefixes_and_ascii_digits() {
        for byte in 0..=u8::MAX {
            for (prefix, expected) in [
                (b"&H".as_slice(), byte.is_ascii_hexdigit()),
                (b"&h".as_slice(), byte.is_ascii_hexdigit()),
                (b"&O".as_slice(), matches!(byte, b'0'..=b'7')),
                (b"&o".as_slice(), matches!(byte, b'0'..=b'7')),
                (b"&".as_slice(), matches!(byte, b'0'..=b'7')),
            ] {
                let source = [prefix, &[byte]].concat();
                assert_eq!(
                    radix_integer_prefix(&source),
                    expected,
                    "{prefix:?}, byte {byte}"
                );
                assert_eq!(
                    binary(&source, 0),
                    (!expected).then_some((Operator::Concatenate, 1))
                );
            }
        }
        for source in [b"&".as_slice(), b"&H", b"&O", b"&h", b"&o"] {
            assert!(!radix_integer_prefix(source));
            assert_eq!(binary(source, 0), Some((Operator::Concatenate, 1)));
        }
    }

    #[test]
    fn expression_precedence_tree_preserves_power_sign_not_groups_and_associativity() {
        let parsed = parse("a ^ b ^ c");
        let NodeKind::Binary(Operator::Power, left, _) = parsed.nodes[parsed.root].kind else {
            panic!("outer left-associative power")
        };
        assert!(matches!(
            parsed.nodes[left].kind,
            NodeKind::Binary(Operator::Power, _, _)
        ));
        assert_eq!(parsed.inventory.work_nodes(), 5);
        let parsed = parse("-a ^ b");
        let NodeKind::Unary(Operator::Negative, power) = parsed.nodes[parsed.root].kind else {
            panic!("sign surrounds power")
        };
        assert!(matches!(
            parsed.nodes[power].kind,
            NodeKind::Binary(Operator::Power, _, _)
        ));
        let parsed = parse("a ^ -b ^ c");
        let NodeKind::Binary(Operator::Power, _, negative) = parsed.nodes[parsed.root].kind else {
            panic!("outer power retains the signed right operand")
        };
        let NodeKind::Unary(Operator::Negative, power) = parsed.nodes[negative].kind else {
            panic!("signed right operand")
        };
        assert!(matches!(
            parsed.nodes[power].kind,
            NodeKind::Binary(Operator::Power, _, _)
        ));
        let parsed = parse("Not a = b And c");
        let NodeKind::Binary(Operator::And, not, _) = parsed.nodes[parsed.root].kind else {
            panic!("logical And root")
        };
        let NodeKind::Unary(Operator::Not, comparison) = parsed.nodes[not].kind else {
            panic!("Not surrounds comparison")
        };
        assert!(matches!(
            parsed.nodes[comparison].kind,
            NodeKind::Binary(Operator::Equal, _, _)
        ));
        let parsed = parse("(a + b) * c");
        let NodeKind::Binary(Operator::Multiply, group, _) = parsed.nodes[parsed.root].kind else {
            panic!("group before multiply")
        };
        let NodeKind::Group(add) = parsed.nodes[group].kind else {
            panic!("retained group")
        };
        assert!(matches!(
            parsed.nodes[add].kind,
            NodeKind::Binary(Operator::Add, _, _)
        ));
        let parsed = parse("a - b - c");
        let NodeKind::Binary(Operator::Subtract, left, _) = parsed.nodes[parsed.root].kind else {
            panic!("left-associative subtraction")
        };
        assert!(matches!(
            parsed.nodes[left].kind,
            NodeKind::Binary(Operator::Subtract, _, _)
        ));
    }

    #[test]
    fn expression_operator_table_and_grouped_comparisons_keep_exact_work() {
        for (spelling, operator) in [
            ("^", Operator::Power),
            ("*", Operator::Multiply),
            ("/", Operator::Divide),
            ("\\", Operator::IntegerDivide),
            ("Mod", Operator::Modulo),
            ("+", Operator::Add),
            ("-", Operator::Subtract),
            ("&", Operator::Concatenate),
            ("=", Operator::Equal),
            ("<>", Operator::NotEqual),
            ("<", Operator::Less),
            ("<=", Operator::LessEqual),
            (">", Operator::Greater),
            (">=", Operator::GreaterEqual),
            ("Is", Operator::Is),
            ("Like", Operator::Like),
            ("And", Operator::And),
            ("Or", Operator::Or),
            ("Xor", Operator::Xor),
            ("Eqv", Operator::Eqv),
            ("Imp", Operator::Imp),
        ] {
            let source = format!("left {spelling} right");
            let parsed = parse(&source);
            assert_eq!(parsed.inventory.work_nodes(), 3, "{source}");
            assert!(
                matches!(parsed.nodes[parsed.root].kind, NodeKind::Binary(actual, _, _) if actual == operator)
            );
            assert_eq!(parsed.inventory.values().len(), 2);
            let source = format!("left {spelling} middle {spelling} right");
            let parsed = parse(&source);
            let NodeKind::Binary(actual, left, _) = parsed.nodes[parsed.root].kind else {
                panic!("repeated binary operator root in {source}")
            };
            assert_eq!(actual, operator);
            assert!(
                matches!(parsed.nodes[left].kind, NodeKind::Binary(actual, _, _) if actual == operator)
            );
            assert_eq!(parsed.inventory.work_nodes(), 5);
        }
        for source in [
            "(a < b) Xor (c > d)",
            "(a = b Xor c <> d)",
            "(a = b Eqv c = d Imp e = f)",
            "a = b And c = d Or e = f",
            "Not Not value",
            "--value + +-2",
            "[Not] + [And] * [Mod]",
            "Notable + Modulus + AndAlso",
        ] {
            assert!(parse(source).inventory.work_nodes() > 0, "{source}");
        }
    }

    #[test]
    fn expression_nested_applications_preserve_source_order_atoms_arguments_and_node_totals() {
        let source = "value(1)=2";
        let parsed = parse(source);
        assert_eq!(parsed.inventory.work_nodes(), 5);
        assert_eq!(parsed.inventory.argument_count(), 1);
        assert!(matches!(
            parsed.nodes[parsed.root].kind,
            NodeKind::Binary(Operator::Equal, _, _)
        ));
        let applications = parsed.inventory.applications();
        assert_eq!(applications.len(), 1);
        assert_eq!(&source[applications[0].application()], "value(1)");
        assert_eq!(&source[applications[0].callee()], "value");
        assert_eq!(&source[applications[0].arguments()], "(1)");
        assert_eq!(
            applications[0].argument_expressions(),
            &[Range { start: 6, end: 7 }]
        );
        assert_eq!(
            parsed
                .inventory
                .values()
                .iter()
                .map(|value| &source[value.span()])
                .collect::<Vec<_>>(),
            ["1", "2"]
        );
        let source = "Outer(value + 1, Inner(other)) * -2";
        let parsed = parse(source);
        assert_eq!(parsed.inventory.work_nodes(), 11);
        assert_eq!(parsed.inventory.argument_count(), 3);
        assert_eq!(
            parsed
                .inventory
                .values()
                .iter()
                .map(|value| &source[value.span()])
                .collect::<Vec<_>>(),
            ["value", "1", "other", "2"]
        );
        let applications = parsed.inventory.applications();
        assert_eq!(applications.len(), 2);
        assert_eq!(&source[applications[0].callee()], "Outer");
        assert_eq!(&source[applications[1].callee()], "Inner");
        assert_eq!(
            &source[applications[0].application()],
            "Outer(value + 1, Inner(other))"
        );
        assert_eq!(
            &source[applications[0].arguments()],
            "(value + 1, Inner(other))"
        );
        assert_eq!(
            applications[0]
                .argument_expressions()
                .iter()
                .map(|span| &source[span.clone()])
                .collect::<Vec<_>>(),
            ["value + 1", "Inner(other)"]
        );
        assert_eq!(
            &source[applications[1].argument_expressions()[0].clone()],
            "other"
        );
        let parsed = parse("Outer(EmptyCall(), \"a,:='Rem \"\"q\"\"\" & \"b\", [True])");
        assert_eq!(parsed.inventory.argument_count(), 3);
        assert!(
            parsed.inventory.applications()[1]
                .argument_expressions()
                .is_empty()
        );
        assert_eq!(
            parsed.inventory.values()[2].kind(),
            ModuleScalarValueKind::Name
        );
    }

    #[test]
    fn expression_inventory_rebases_every_range_and_preserves_logical_trivia() {
        for newline in ["\n", "\r\n"] {
            let text = format!(
                " ( _{newline} [Outer] _{newline} (value _{newline} + 1, [Inner]( _{newline} other _{newline} )) ) * -2 "
            );
            let prefix = b"prefix\r\n";
            let source = [prefix.as_slice(), text.as_bytes(), b"tail"].concat();
            let range = prefix.len()..prefix.len() + text.len();
            let (span, inventory) = super::inventory(
                &source[range.clone()],
                0,
                &range,
                text.len(),
                text.len(),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .unwrap();
            assert_eq!(span.start, prefix.len() + 1);
            assert_eq!(span.end, range.end - 1);
            assert_eq!(inventory.work_nodes(), 12);
            assert_eq!(inventory.argument_count(), 3);
            for value in inventory.values() {
                assert!(span.start <= value.span().start && value.span().end <= span.end);
            }
            for application in inventory.applications() {
                assert_eq!(source[application.arguments().start], b'(');
                assert_eq!(source[application.arguments().end - 1], b')');
                assert!(application.application().start >= range.start);
                for argument in application.argument_expressions() {
                    assert!(
                        application.arguments().start < argument.start
                            && argument.end < application.arguments().end
                    );
                }
            }
        }
    }

    #[test]
    fn expression_node_and_nested_argument_ceilings_are_exact_zero_and_one_short() {
        let source = b"Outer(value + 1, Inner(other)) * -2";
        for nodes in [11, 12] {
            assert!(
                Parser::new(source, 0, nodes, 3)
                    .parse(&mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_some()
            );
        }
        for nodes in [0, 1, 10] {
            assert!(matches!(
                Parser::new(source, 0, nodes, 3).parse(&mut || Ok::<(), ()>(())),
                Err(ModuleError::ExpressionLimit)
            ));
        }
        for arguments in [0, 1, 2] {
            assert!(matches!(
                Parser::new(source, 0, 11, arguments).parse(&mut || Ok::<(), ()>(())),
                Err(ModuleError::ArgumentLimit)
            ));
        }
        let empty = parse("EmptyCall() + 1");
        assert_eq!(empty.inventory.work_nodes(), 4);
        assert_eq!(empty.inventory.argument_count(), 0);
        assert!(
            Parser::new(b"EmptyCall() + 1", 0, 4, 0)
                .parse(&mut || Ok::<(), ()>(()))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn expression_deep_groups_nested_apps_and_wide_chains_use_flat_bounded_storage() {
        let depth = 2_048;
        let source = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
        let parsed = parse(&source);
        assert_eq!(parsed.inventory.work_nodes(), depth + 1);
        assert_eq!(parsed.inventory.values().len(), 1);
        let count = 4_096;
        let source = vec!["1"; count].join("+");
        let parsed = parse(&source);
        assert_eq!(parsed.inventory.work_nodes(), count * 2 - 1);
        assert_eq!(parsed.inventory.values().len(), count);
        let depth = 256;
        let source = format!("{}1{}", "F(".repeat(depth), ")".repeat(depth));
        let parsed = parse(&source);
        assert_eq!(parsed.inventory.work_nodes(), depth * 2 + 1);
        assert_eq!(parsed.inventory.argument_count(), depth);
        assert_eq!(parsed.inventory.applications().len(), depth);
        assert!(
            parsed
                .inventory
                .applications()
                .windows(2)
                .all(|pair| pair[0].callee().start < pair[1].callee().start)
        );
    }

    #[test]
    fn expression_invalid_delimiters_operators_and_richer_atoms_have_no_inventory() {
        for source in [
            "",
            " ",
            "()",
            "(1",
            "1)",
            "1+",
            "+",
            "Not",
            "*1",
            "1**2",
            "1^^2",
            "F(,1)",
            "F(1,)",
            "F(1,,2)",
            "F(value:=1)",
            "F(ByVal value)",
            "F(ByRef value)",
            "F(1)(2)",
            "F(1).member",
            "holder.F(1)",
            "Line(1)+2",
            "line()",
            "1.5e+ +2",
            "1E+ +2",
            "1.5+2.5d",
            "#1/1/2000#",
            "value$+1",
            "Me+1",
            "Nothing+1",
            "\"open",
            "\"physical\nline\"",
            "\"continued _\n",
            "1;2",
            "1 extra",
            "1\0+2",
            "1\n+2",
            "1\r+2",
            "1\x0b+2",
            "1\x0c+2",
            "[open",
            "[]",
            "[space name]",
            "1Mod2",
            "a AndAlso b",
            "Notable value",
        ] {
            assert!(
                Parser::new(source.as_bytes(), 0, source.len(), source.len())
                    .parse(&mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none(),
                "{source:?}"
            );
        }
        assert!(parse("[Line](1)+2").inventory.work_nodes() > 0);
    }

    #[test]
    fn expression_comparison_chains_are_left_associative_without_type_inference() {
        for (source, inner, outer) in [
            ("a<b<c", Operator::Less, Operator::Less),
            ("a=b=c", Operator::Equal, Operator::Equal),
            ("a Is b Like c", Operator::Is, Operator::Like),
        ] {
            let parsed = parse(source);
            let NodeKind::Binary(actual, left, _) = parsed.nodes[parsed.root].kind else {
                panic!("outer comparison in {source}")
            };
            assert_eq!(actual, outer);
            assert!(
                matches!(parsed.nodes[left].kind, NodeKind::Binary(actual, _, _) if actual == inner)
            );
            assert_eq!(parsed.inventory.work_nodes(), 5);
            assert_eq!(parsed.span, 0..source.len());
            assert_eq!(parsed.inventory.argument_count(), 0);
            assert!(parsed.inventory.applications().is_empty());
            assert_eq!(
                parsed
                    .inventory
                    .values()
                    .iter()
                    .map(|value| &source[value.span()])
                    .collect::<Vec<_>>(),
                ["a", "b", "c"]
            );
        }
        for (spelling, operator) in [
            ("And", Operator::And),
            ("Or", Operator::Or),
            ("Xor", Operator::Xor),
            ("Eqv", Operator::Eqv),
            ("Imp", Operator::Imp),
        ] {
            let source = format!("a<b {spelling} c>d");
            let parsed = parse(&source);
            let NodeKind::Binary(actual, left, right) = parsed.nodes[parsed.root].kind else {
                panic!("logical root in {source}")
            };
            assert_eq!(actual, operator);
            assert!(matches!(
                parsed.nodes[left].kind,
                NodeKind::Binary(Operator::Less, _, _)
            ));
            assert!(matches!(
                parsed.nodes[right].kind,
                NodeKind::Binary(Operator::Greater, _, _)
            ));
            assert_eq!(parsed.inventory.work_nodes(), 7);
            assert_eq!(parsed.span, 0..source.len());
            assert_eq!(
                parsed
                    .inventory
                    .values()
                    .iter()
                    .map(|value| &source[value.span()])
                    .collect::<Vec<_>>(),
                ["a", "b", "c", "d"]
            );
        }
        let source = "a<b Xor c>d Eqv e=f Imp g=h";
        let parsed = parse(source);
        let NodeKind::Binary(Operator::Imp, equivalence, last) = parsed.nodes[parsed.root].kind
        else {
            panic!("Imp has the lowest precedence")
        };
        assert!(matches!(
            parsed.nodes[last].kind,
            NodeKind::Binary(Operator::Equal, _, _)
        ));
        let NodeKind::Binary(Operator::Eqv, exclusive, _) = parsed.nodes[equivalence].kind else {
            panic!("Eqv surrounds higher-precedence Xor")
        };
        assert!(matches!(
            parsed.nodes[exclusive].kind,
            NodeKind::Binary(Operator::Xor, _, _)
        ));
        assert_eq!(parsed.inventory.work_nodes(), 15);
        assert_eq!(parsed.span, 0..source.len());
    }

    #[test]
    fn expression_ungrouped_comparison_and_or_chain_preserves_operator_precedence() {
        let source = "value < 1 Or other > 2 And third < 3";
        let parsed = parse(source);
        let NodeKind::Binary(Operator::Or, left, right) = parsed.nodes[parsed.root].kind else {
            panic!("Or surrounds higher-precedence And")
        };
        assert!(matches!(
            parsed.nodes[left].kind,
            NodeKind::Binary(Operator::Less, _, _)
        ));
        let NodeKind::Binary(Operator::And, middle, last) = parsed.nodes[right].kind else {
            panic!("right And retains both comparisons")
        };
        assert!(matches!(
            parsed.nodes[middle].kind,
            NodeKind::Binary(Operator::Greater, _, _)
        ));
        assert!(matches!(
            parsed.nodes[last].kind,
            NodeKind::Binary(Operator::Less, _, _)
        ));
        assert_eq!(parsed.span, 0..source.len());
        assert_eq!(parsed.inventory.work_nodes(), 11);
        assert_eq!(parsed.inventory.argument_count(), 0);
        assert!(parsed.inventory.applications().is_empty());
        assert_eq!(
            parsed
                .inventory
                .values()
                .iter()
                .map(|value| &source[value.span()])
                .collect::<Vec<_>>(),
            ["value", "1", "other", "2", "third", "3"]
        );
    }

    #[test]
    fn expression_word_operator_all_byte_boundaries_never_accept_prefix_keywords() {
        for byte in 0..=u8::MAX {
            for (prefix, suffix, expected) in [
                (
                    b"a And".as_slice(),
                    b"b".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'+' | b'-'),
                ),
                (
                    b"Not",
                    b"(b)",
                    byte.is_ascii_alphanumeric()
                        || matches!(byte, b'_' | b' ' | b'\t' | b'+' | b'-'),
                ),
                (
                    b"a+b",
                    b"",
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b' ' | b'\t'),
                ),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let parsed = Parser::new(&source, 0, source.len(), source.len())
                    .parse(&mut || Ok::<(), ()>(()))
                    .unwrap();
                assert_eq!(
                    parsed.is_some(),
                    expected,
                    "{prefix:?} byte {byte} {suffix:?}"
                );
                if let Some(parsed) = parsed {
                    assert!(parsed.span.start < parsed.span.end && parsed.span.end <= source.len());
                    for value in parsed.inventory.values() {
                        assert!(
                            value.span().start < value.span().end
                                && value.span().end <= source.len()
                        );
                    }
                    if prefix == b"Not" && (byte.is_ascii_alphanumeric() || byte == b'_') {
                        assert_eq!(parsed.inventory.applications().len(), 1);
                        assert_eq!(parsed.inventory.applications()[0].callee(), 0..4);
                        assert_eq!(parsed.inventory.work_nodes(), 3);
                        assert!(matches!(
                            parsed.nodes[parsed.root].kind,
                            NodeKind::Application(_)
                        ));
                    }
                }
            }
        }
    }

    #[test]
    fn expression_adjacent_numeric_and_name_type_suffixes_do_not_become_operators() {
        for (atom, suffixes) in [("2!", b"%!#@$".as_slice()), ("value", b"%&^!#@$")] {
            for suffix in suffixes {
                for tail in ["", "3", "other", "(3)", " + 3"] {
                    let source = format!("{atom}{}{tail}", char::from(*suffix));
                    assert!(
                        Parser::new(source.as_bytes(), 0, source.len(), source.len())
                            .parse(&mut || Ok::<(), ()>(()))
                            .unwrap()
                            .is_none(),
                        "{source}"
                    );
                }
            }
        }
        for source in [
            "2^3", "2&3", "a^b", "a&b", "-2^3", "-a^b", "F(2^3)+1", "F(a&b)+1",
        ] {
            assert!(
                Parser::new(source.as_bytes(), 0, source.len(), source.len())
                    .parse(&mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn expression_unambiguous_separated_or_closed_operands_keep_power_and_concatenation() {
        for (source, work, arguments) in [
            ("2 ^ 3", 3, 0),
            ("2 & 3", 3, 0),
            ("a ^ b", 3, 0),
            ("a & b", 3, 0),
            ("[a]^[b]", 3, 0),
            ("[a]&[b]", 3, 0),
            ("(a)^b", 4, 0),
            ("(a)&b", 4, 0),
            ("F()^b", 4, 0),
            ("F()&b", 4, 0),
            ("F(1)^2", 5, 1),
            ("\"a\"&\"b\"", 3, 0),
            ("True^False", 3, 0),
            ("True&False", 3, 0),
            ("2 _\r\n ^ 3", 3, 0),
            ("a _\n & b", 3, 0),
        ] {
            let parsed = parse(source);
            assert_eq!(parsed.span, 0..source.len());
            assert_eq!(parsed.inventory.work_nodes(), work, "{source}");
            assert_eq!(parsed.inventory.argument_count(), arguments, "{source}");
            for value in parsed.inventory.values() {
                assert!(value.span().start < value.span().end && value.span().end <= source.len());
            }
        }
    }

    #[test]
    fn expression_numeric_and_name_all_byte_boundaries_preserve_atomic_lexical_identity() {
        for byte in 0..=u8::MAX {
            let binary = matches!(byte, b'+' | b'-' | b'*' | b'/' | b'\\' | b'=' | b'<' | b'>');
            for (prefix, suffix, atom_continuation) in [
                (
                    b"2".as_slice(),
                    b"3".as_slice(),
                    byte.is_ascii_digit() || matches!(byte, b'.' | b'E' | b'e' | b'D' | b'd'),
                ),
                (b"a", b"b", byte.is_ascii_alphanumeric() || byte == b'_'),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let parsed = Parser::new(&source, 0, source.len(), source.len())
                    .parse(&mut || Ok::<(), ()>(()))
                    .unwrap();
                assert_eq!(
                    parsed.is_some(),
                    atom_continuation || binary,
                    "{prefix:?} byte {byte}"
                );
                if let Some(parsed) = parsed {
                    assert_eq!(parsed.span, 0..source.len());
                    assert_eq!(
                        parsed.inventory.work_nodes(),
                        if atom_continuation { 1 } else { 3 }
                    );
                    assert_eq!(
                        parsed.inventory.values().len(),
                        if atom_continuation { 1 } else { 2 }
                    );
                    assert!(parsed.inventory.applications().is_empty());
                    assert_eq!(parsed.inventory.argument_count(), 0);
                }
            }
        }
    }

    #[test]
    fn expression_cancellation_at_every_scan_reserve_reduce_and_rebase_checkpoint_is_typed() {
        let source = b"(Outer(value + 007, Inner(\"a\"\"b\" & [True]))) * -2";
        let range = 9..9 + source.len();
        let mut checkpoints = 0;
        super::inventory(source, 0, &range, source.len(), source.len(), &mut || {
            checkpoints += 1;
            Ok::<(), ()>(())
        })
        .unwrap()
        .unwrap();
        for cancelled_at in 1..=checkpoints {
            let mut calls = 0;
            assert!(
                matches!(
                    super::inventory(source, 0, &range, source.len(), source.len(), &mut || {
                        calls += 1;
                        if calls == cancelled_at {
                            Err("stop")
                        } else {
                            Ok(())
                        }
                    }),
                    Err(ModuleError::Cancelled("stop"))
                ),
                "checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
    }

    #[test]
    fn expression_storage_overflow_and_reservation_cancellation_do_not_allocate_unboundedly() {
        let mut nodes = Vec::<Node>::new();
        assert_eq!(
            reserve_exact(&mut nodes, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert!(nodes.is_empty());
        assert_eq!(
            reserve(&mut nodes, 0, Ceiling::Nodes, &mut || Ok::<(), ()>(())),
            Err(ModuleError::ExpressionLimit)
        );
        for stop in 1..=2 {
            let mut calls = 0;
            let mut nodes = Vec::<Node>::new();
            assert_eq!(
                reserve_exact(&mut nodes, 1, &mut || {
                    calls += 1;
                    if calls == stop { Err("stop") } else { Ok(()) }
                }),
                Err(ModuleError::Cancelled("stop"))
            );
            assert!(nodes.is_empty());
        }
    }

    #[test]
    fn explicit_call_private_root_proofs_and_source_ranges_fail_closed() {
        let source = b"Target(1+2)";
        let read = |source: &[u8], callee, range| {
            expression_call(
                source,
                callee,
                range,
                source.len(),
                source.len(),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
        };
        let call = read(source, 0..6, 7..7 + source.len()).unwrap();
        assert_eq!(call.callee(), 7..13);
        assert_eq!(call.arguments(), 13..7 + source.len());
        assert_eq!(
            call.inventory().applications()[0].application(),
            7..7 + source.len()
        );
        for callee in [
            0..5,
            0..7,
            1..6,
            2..6,
            3..6,
            4..6,
            5..6,
            0..0,
            0..source.len() + 1,
        ] {
            assert!(read(source, callee, 0..source.len()).is_none());
        }
        for (source, callee) in [
            (b"prefixTarget(1+2)".as_slice(), 6..12),
            (b"_Target(1+2)", 1..7),
            (b"1Target(1+2)", 1..7),
        ] {
            assert!(read(source, callee, 0..source.len()).is_none());
        }
        for (source, callee) in [
            (b"Call Target(1+2)".as_slice(), 5..11),
            (b"Call\tTarget(1+2)", 5..11),
            (b"Call _\r\n Target(1+2)", 9..15),
            (b"Call _\n [Target](1+2)", 8..16),
        ] {
            let call = read(source, callee.clone(), 0..source.len()).unwrap();
            assert_eq!(call.callee(), callee);
            assert_eq!(call.inventory().work_nodes(), 5);
        }
        for range in [
            0..source.len() - 1,
            0..source.len() + 1,
            Range {
                start: usize::MAX,
                end: 0,
            },
        ] {
            assert!(read(source, 0..6, range).is_none());
        }
        for source in [
            b"Target(1)+2".as_slice(),
            b"Target(1)(2)",
            b"Target(1)=2",
            b"Target(1) trailing",
            b"Target",
            b"Target+Inner(1)",
        ] {
            assert!(read(source, 0..6, 0..source.len()).is_none());
        }
        let offset = usize::MAX - source.len();
        let call = read(source, 0..6, offset..usize::MAX).unwrap();
        assert_eq!(call.statement().end, usize::MAX);
        assert_eq!(call.arguments().end, usize::MAX);
        assert_eq!(call.argument_expressions()[0].end, usize::MAX - 1);
    }

    #[test]
    fn explicit_call_deep_applications_use_bounded_iterative_inventory_and_drop() {
        let depth = 2048;
        let source = format!(
            "Target({}value{})",
            "Inner(".repeat(depth),
            ")".repeat(depth)
        );
        let maximum_nodes = 2 * (depth + 1) + 1;
        let maximum_arguments = depth + 1;
        let read = |maximum_nodes, maximum_arguments| {
            expression_call(
                source.as_bytes(),
                0..6,
                0..source.len(),
                maximum_nodes,
                maximum_arguments,
                &mut || Ok::<(), ()>(()),
            )
        };
        let call = read(maximum_nodes, maximum_arguments).unwrap().unwrap();
        assert_eq!(call.inventory().work_nodes(), maximum_nodes);
        assert_eq!(call.inventory().argument_count(), maximum_arguments);
        assert_eq!(call.inventory().applications().len(), depth + 1);
        assert_eq!(call.inventory().values().len(), 1);
        assert_eq!(call.argument_expressions().len(), 1);
        assert_eq!(
            call.inventory().applications()[0].application(),
            0..source.len()
        );
        assert_eq!(
            read(maximum_nodes - 1, maximum_arguments),
            Err(ModuleError::ExpressionLimit)
        );
        assert_eq!(
            read(maximum_nodes, maximum_arguments - 1),
            Err(ModuleError::ArgumentLimit)
        );
        drop(call);
    }

    #[test]
    fn expression_corrupted_private_indices_fail_closed_without_synthetic_ranges() {
        let mut parser = Parser::new(b")", 0, 8, 8);
        assert!(matches!(
            parser.argument(0, 0, &mut || Ok::<(), ()>(())),
            Err(ModuleError::Unsupported { .. })
        ));
        parser.nodes.push(Node {
            kind: NodeKind::Callee,
            span: 0..1,
        });
        assert!(matches!(
            parser.argument(0, 0, &mut || Ok::<(), ()>(())),
            Err(ModuleError::Unsupported { .. })
        ));
        parser.frames.push(Frame {
            kind: FrameKind::Root,
            value_base: 0,
            operator_base: 0,
            has_argument: false,
        });
        parser.frames.push(Frame {
            kind: FrameKind::Application {
                node: 9,
                application: 0,
            },
            value_base: 0,
            operator_base: 0,
            has_argument: false,
        });
        assert!(matches!(
            parser.close(9, Some(0), &mut || Ok::<(), ()>(())),
            Err(ModuleError::Unsupported { .. })
        ));
        assert_eq!(parser.cursor, 0);
        assert!(matches!(
            parser.close(0, Some(0), &mut || Ok::<(), ()>(())),
            Err(ModuleError::Unsupported { .. })
        ));
        assert_eq!(parser.cursor, 0);
        assert!(
            super::inventory(
                b"1+2",
                0,
                &Range {
                    start: usize::MAX,
                    end: 0
                },
                3,
                0,
                &mut || Ok::<(), ()>(())
            )
            .unwrap()
            .is_none()
        );
    }
}
