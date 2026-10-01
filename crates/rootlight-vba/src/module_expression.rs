//! Iterative, source-bound expression inventory for procedural assignments.
//! Flat temporary nodes preserve precedence without recursive parsing or dropping.
//! The retained inventory is syntax only, not evaluation, type or binding evidence.

use std::ops::Range;

use super::{
    logical_whitespace_len,
    module::ModuleError,
    module_body::{ModuleScalarValue, ModuleScalarValueKind, atomic_value},
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

/// Flat source records from one completely admitted expression.
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

/// One source-written simple-name application within an expression.
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
    /// Returns the exact callee-through-closing-parenthesis source span.
    #[must_use]
    pub fn application(&self) -> Range<usize> {
        self.application.clone()
    }

    /// Returns the exact callee name, retaining authored brackets.
    #[must_use]
    pub fn callee(&self) -> Range<usize> {
        self.callee.clone()
    }

    /// Returns the original parentheses, including argument bytes and trivia.
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
}

struct Parsed {
    inventory: ModuleExpressionInventory,
    span: Range<usize>,
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
        self.frames.push(Frame {
            kind: FrameKind::Root,
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
                // ordinary identifiers. An unsupported typed token is not a
                // binary operator merely because its suffix shares that spelling.
                let adjacent_suffix = match value.kind() {
                    ModuleScalarValueKind::UnsignedDecimal => matches!(
                        self.source.get(self.cursor),
                        Some(b'%' | b'&' | b'^' | b'!' | b'#' | b'@')
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
                let FrameKind::Application { application, .. } = frame.kind else {
                    return Ok(None);
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
                    FrameKind::Root => return Ok(None),
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
                let Some(root) = self.finish_segment(check)? else {
                    return Ok(None);
                };
                let Some(span) = self.nodes.get(root).map(|node| node.span.clone()) else {
                    return Ok(None);
                };
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
        b'&' => Some(Operator::Concatenate),
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
    let Some(mut parsed) =
        Parser::new(source, start, maximum_nodes, maximum_arguments).parse(check)?
    else {
        return Ok(None);
    };
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
    Ok(Some((parsed.span, parsed.inventory)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Parsed {
        Parser::new(source.as_bytes(), 0, source.len(), source.len())
            .parse(&mut || Ok::<(), ()>(()))
            .unwrap()
            .expect("complete expression")
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
            "1.5+2",
            "1E2+2",
            "&H1+2",
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
        for (atom, suffixes) in [("2", b"%&^!#@".as_slice()), ("value", b"%&^!#@$")] {
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
                (b"2".as_slice(), b"3".as_slice(), byte.is_ascii_digit()),
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
