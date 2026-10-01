//! Complete local declarations, expression assignments and explicit calls for the
//! closed first-party module inventory.
//! Records own only original byte ranges; declaration identity and reference roles
//! remain obligations of the adapter's existing structural normalization boundary.

use std::ops::Range;

use super::{logical_whitespace_len, module::ModuleError, module_expression, name};

/// A completely admitted local statement inside one procedural body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleBodyStatement {
    /// One explicitly typed scalar Dim or Const declaration.
    Declaration(ModuleLocalDeclaration),
    /// One complete Dim list retaining each local scalar or numeric-array declarator.
    Variables(ModuleLocalVariables),
    /// One simple scalar assignment, optionally introduced by Let.
    Assignment(ModuleScalarAssignment),
    /// A simple destination assigned one complete parenthesized scalar application.
    ApplicationAssignment(ModuleApplicationAssignment),
    /// A simple destination assigned one completely admitted structured expression.
    ExpressionAssignment(module_expression::ModuleExpressionAssignment),
    /// One explicit Call with a simple callee and a complete scalar argument list.
    Call(ModuleCall),
    /// One explicit Call with a complete parenthesized positional expression list.
    ExpressionCall(module_expression::ModuleExpressionCall),
}

impl ModuleBodyStatement {
    /// Returns the exact trimmed logical statement, excluding trailing comments.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        match self {
            Self::Declaration(declaration) => declaration.statement(),
            Self::Variables(variables) => variables.statement(),
            Self::Assignment(assignment) => assignment.statement(),
            Self::ApplicationAssignment(assignment) => assignment.statement(),
            Self::ExpressionAssignment(assignment) => assignment.statement(),
            Self::Call(call) => call.statement(),
            Self::ExpressionCall(call) => call.statement(),
        }
    }
}

/// The written category of an explicitly typed scalar local declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleLocalKind {
    /// A local Dim variable without an initializer.
    Variable,
    /// A local Const with a literal or single-minus numeric initializer.
    Constant,
}

/// One scalar local declaration with exact original source boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleLocalDeclaration {
    kind: ModuleLocalKind,
    statement: Range<usize>,
    declarator: Range<usize>,
    name: Range<usize>,
    type_range: Range<usize>,
    value: Option<ModuleScalarValue>,
}

impl ModuleLocalDeclaration {
    /// Returns whether this is a written Dim variable or Const.
    #[must_use]
    pub const fn kind(&self) -> ModuleLocalKind {
        self.kind
    }

    /// Returns the whole trimmed statement, including Dim or Const.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }

    /// Returns the declaration from its name through its type or initializer.
    /// The Dim/Const prefix is excluded to preserve structural declaration identity.
    #[must_use]
    pub fn declarator(&self) -> Range<usize> {
        self.declarator.clone()
    }

    /// Returns the exact ASCII name token, retaining authored brackets.
    #[must_use]
    pub fn name(&self) -> Range<usize> {
        self.name.clone()
    }

    /// Returns the exact intrinsic scalar type keyword, excluding As and trivia.
    #[must_use]
    pub fn type_range(&self) -> Range<usize> {
        self.type_range.clone()
    }

    /// Returns the initializer atom for Const, or None for Dim.
    /// A numeric unary minus is retained separately from the atom's token span.
    #[must_use]
    pub fn value(&self) -> Option<&ModuleScalarValue> {
        self.value.as_ref()
    }
}

/// One complete local Dim statement with independently authored declarators.
///
/// Variables stay in source order and belong to the containing procedure, never
/// the module-variable inventory. A comma list does not distribute one sibling's
/// written type to another or establish runtime sizes or uniqueness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleLocalVariables {
    statement: Range<usize>,
    variables: Vec<ModuleLocalVariable>,
}

impl ModuleLocalVariables {
    /// Returns the complete trimmed Dim statement, shared by every sibling.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }

    /// Returns every complete local declarator in authored source order.
    #[must_use]
    pub fn variables(&self) -> &[ModuleLocalVariable] {
        &self.variables
    }
}

/// One uninitialized local variable with exact source-written declaration ranges.
///
/// An absent As clause does not fabricate a Variant token. Array syntax retains
/// numeric bounds without evaluating magnitudes, allocation size or lower defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleLocalVariable {
    syntax: super::module::VariableDeclarator,
}

impl ModuleLocalVariable {
    /// Returns the exact name through written bounds or type, excluding Dim.
    #[must_use]
    pub fn declarator(&self) -> Range<usize> {
        self.syntax.declarator.clone()
    }

    /// Returns the exact authored local name, retaining brackets when present.
    #[must_use]
    pub fn name(&self) -> Range<usize> {
        self.syntax.name.clone()
    }

    /// Returns complete numeric array parentheses, or None for scalar syntax.
    #[must_use]
    pub fn array_bounds(&self) -> Option<Range<usize>> {
        self.syntax.array_bounds.clone()
    }

    /// Returns logical group, dimension, endpoint and written-sign work for bounds.
    /// Scalar syntax costs zero; this count is not an evaluated array size.
    #[must_use]
    pub const fn array_work_nodes(&self) -> usize {
        self.syntax.array_work_nodes
    }

    /// Returns the written intrinsic type keyword, or None without an As clause.
    #[must_use]
    pub fn type_range(&self) -> Option<Range<usize>> {
        self.syntax.type_range.clone()
    }
}

/// One complete scalar assignment to a simple source-written name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleScalarAssignment {
    statement: Range<usize>,
    destination: Range<usize>,
    value: ModuleScalarValue,
}

impl ModuleScalarAssignment {
    /// Returns the whole trimmed statement, including an optional Let prefix.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }

    /// Returns the destination's exact name token, excluding Let and trivia.
    #[must_use]
    pub fn destination(&self) -> Range<usize> {
        self.destination.clone()
    }

    /// Returns the complete atomic right-hand value, without evaluation or binding.
    #[must_use]
    pub fn value(&self) -> &ModuleScalarValue {
        &self.value
    }
}

/// A complete assignment whose right-hand syntax is one simple-name application.
///
/// Parentheses contain zero or more positional scalar atoms. This source inventory
/// does not distinguish procedure application from array indexing or establish
/// target identity, argument correspondence, arity or runtime type compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleApplicationAssignment {
    statement: Range<usize>,
    destination: Range<usize>,
    application: Range<usize>,
    callee: Range<usize>,
    arguments: Range<usize>,
    argument_values: Vec<ModuleScalarValue>,
}

impl ModuleApplicationAssignment {
    /// Returns the complete trimmed statement, including an optional Let prefix.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }

    /// Returns the exact destination name, excluding Let and surrounding trivia.
    #[must_use]
    pub fn destination(&self) -> Range<usize> {
        self.destination.clone()
    }

    /// Returns the application from the callee name through the closing parenthesis.
    #[must_use]
    pub fn application(&self) -> Range<usize> {
        self.application.clone()
    }

    /// Returns the exact simple callee name, retaining authored brackets.
    #[must_use]
    pub fn callee(&self) -> Range<usize> {
        self.callee.clone()
    }

    /// Returns the required authored parentheses, including arguments and trivia.
    #[must_use]
    pub fn arguments(&self) -> Range<usize> {
        self.arguments.clone()
    }

    /// Returns all retained positional scalar atoms in original source order.
    /// An empty slice records empty syntax, not a proven zero-arity target.
    #[must_use]
    pub fn argument_values(&self) -> &[ModuleScalarValue] {
        &self.argument_values
    }
}

/// One complete explicit Call retaining syntax without establishing a target.
///
/// The callee is a simple ASCII name, optionally bracketed. Parentheses contain
/// zero or more complete positional scalar atoms. Target identity, parameter
/// correspondence, arity and type compatibility remain separate binding obligations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleCall {
    statement: Range<usize>,
    callee: Range<usize>,
    arguments: Option<Range<usize>>,
    argument_values: Vec<ModuleScalarValue>,
}

/// A source-compatible legacy spelling for [`ModuleCall`].
///
/// Values using this spelling can now contain nonempty scalar argument lists.
/// Inspect [`ModuleCall::argument_values`] rather than inferring zero arity.
pub type ModuleZeroArgumentCall = ModuleCall;

impl ModuleCall {
    /// Returns the complete trimmed statement, including the Call keyword.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }

    /// Returns the exact callee name token, retaining authored brackets.
    #[must_use]
    pub fn callee(&self) -> Range<usize> {
        self.callee.clone()
    }

    /// Returns the authored parentheses, including their argument bytes and trivia.
    ///
    /// None means no parentheses were written, not that the target is callable.
    #[must_use]
    pub fn arguments(&self) -> Option<Range<usize>> {
        self.arguments.clone()
    }

    /// Returns every complete positional scalar atom in authored source order.
    /// An empty slice covers bare or explicitly empty Call syntax. These records
    /// prove neither formal-parameter identity nor runtime type compatibility.
    #[must_use]
    pub fn argument_values(&self) -> &[ModuleScalarValue] {
        &self.argument_values
    }
}

/// Syntax categories admitted as complete atomic scalar values.
/// `Unsigned` describes the atom token, not a proven nonnegative runtime value.
/// An initializer's authored unary minus is retained separately from that token.
/// Numeric categories may carry an authored [`ModuleIntegerSuffix`] or [`ModuleFloatSuffix`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleScalarValueKind {
    /// One or more decimal digits without sign, separator or exponent.
    /// An optional authored integer suffix is retained separately.
    UnsignedDecimal,
    /// An authored `&H`/`&h` prefix and ASCII hexadecimal digits.
    /// The spelling is retained without magnitude, signed-value or type evaluation.
    UnsignedHexadecimal,
    /// An authored `&`, `&O` or `&o` prefix and octal digits.
    /// The spelling is retained without magnitude, signed-value or type evaluation.
    UnsignedOctal,
    /// An authored decimal FLOAT token with a point, exponent or FLOAT suffix.
    /// The spelling is retained without value, rounding, range or type evaluation.
    UnsignedFloat,
    /// One physically closed string, with optional doubled-quote escapes.
    String,
    /// An unbracketed True or False keyword, without coercion or evaluation.
    Boolean,
    /// One ASCII name, retaining authored brackets and requiring later binding.
    Name,
}

/// One authored INTEGER literal suffix, without magnitude or runtime-type evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleIntegerSuffix {
    /// The authored `%` marker.
    Percent,
    /// The authored `&` marker.
    Ampersand,
    /// The authored `^` marker.
    Caret,
}

impl ModuleIntegerSuffix {
    /// Returns the exact ASCII suffix marker written in the original token.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Percent => '%',
            Self::Ampersand => '&',
            Self::Caret => '^',
        }
    }
}

/// One authored FLOAT literal suffix, without value or runtime-type evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleFloatSuffix {
    /// The authored `!` marker.
    Exclamation,
    /// The authored `#` marker.
    Hash,
    /// The authored `@` marker.
    At,
}

impl ModuleFloatSuffix {
    /// Returns the exact ASCII suffix marker written in the original token.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Exclamation => '!',
            Self::Hash => '#',
            Self::At => '@',
        }
    }
}

/// One complete source atom with optional initializer-only unary-minus evidence.
/// The atom's category, suffixes and token range do not evaluate its runtime value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleScalarValue {
    kind: ModuleScalarValueKind,
    integer_suffix: Option<ModuleIntegerSuffix>,
    float_suffix: Option<ModuleFloatSuffix>,
    unary_minus: Option<Range<usize>>,
    span: Range<usize>,
}

impl ModuleScalarValue {
    /// Returns the value's syntax category, not its runtime type or target identity.
    #[must_use]
    pub const fn kind(&self) -> ModuleScalarValueKind {
        self.kind
    }

    /// Returns the optional authored INTEGER suffix, never an inferred runtime type.
    /// Noninteger atoms and integers with no written suffix return None.
    #[must_use]
    pub const fn integer_suffix(&self) -> Option<ModuleIntegerSuffix> {
        self.integer_suffix
    }

    /// Returns the optional authored FLOAT suffix, never an inferred runtime type.
    /// Other atoms and FLOAT tokens with no written suffix return None.
    #[must_use]
    pub const fn float_suffix(&self) -> Option<ModuleFloatSuffix> {
        self.float_suffix
    }

    /// Returns the exact atom token, including a numeric suffix or both string quotes.
    /// An initializer's unary minus and intervening trivia are excluded.
    #[must_use]
    pub fn span(&self) -> Range<usize> {
        self.span.clone()
    }

    /// Returns the exact authored unary minus of a numeric initializer, if present.
    /// General expression operators are retained by their separate expression inventory.
    #[must_use]
    pub fn unary_minus_span(&self) -> Option<Range<usize>> {
        self.unary_minus.clone()
    }

    /// Returns the complete initializer from its unary minus or atom through the token end.
    /// Authored intervening trivia is retained; equals and surrounding trivia are excluded.
    #[must_use]
    pub fn initializer_span(&self) -> Range<usize> {
        self.unary_minus
            .as_ref()
            .map_or(self.span.start, |minus| minus.start)..self.span.end
    }

    /// Returns extra initializer work beyond the preserved legacy atom path.
    /// An authored unary minus costs one node; all other atoms cost zero extra nodes.
    /// Exponent signs are part of a FLOAT token and do not add operator work.
    #[must_use]
    pub const fn initializer_work_nodes(&self) -> usize {
        if self.unary_minus.is_some() { 1 } else { 0 }
    }

    pub(super) fn rebase(&mut self, source_range: &Range<usize>) {
        self.span = absolute(source_range, self.span.clone());
        if let Some(minus) = &mut self.unary_minus {
            *minus = absolute(source_range, minus.clone());
        }
    }
}

#[cfg(test)]
fn statement<E>(
    source: &[u8],
    range: Range<usize>,
    maximum_arguments: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleBodyStatement>, ModuleError<E>> {
    statement_with_expression_limits(source, range, maximum_arguments, source.len(), check)
}

#[cfg(test)]
pub(super) fn statement_with_expression_limits<E>(
    source: &[u8],
    range: Range<usize>,
    maximum_arguments: usize,
    maximum_expression_nodes: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleBodyStatement>, ModuleError<E>> {
    statement_with_limits(
        source,
        range,
        BodyLimits {
            arguments: maximum_arguments,
            expression_nodes: maximum_expression_nodes,
            records: source.len(),
        },
        check,
    )
}

#[derive(Clone, Copy)]
pub(super) struct BodyLimits {
    pub(super) arguments: usize,
    pub(super) expression_nodes: usize,
    pub(super) records: usize,
}

pub(super) fn statement_with_limits<E>(
    source: &[u8],
    range: Range<usize>,
    limits: BodyLimits,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleBodyStatement>, ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    let text = source.get(range.clone()).unwrap_or_default();
    let mut cursor = 0;
    let first = word(text, &mut cursor, check).map_err(ModuleError::Cancelled)?;
    if first.eq_ignore_ascii_case(b"Call") {
        return explicit_call(
            text,
            range,
            cursor,
            limits.arguments,
            limits.expression_nodes,
            check,
        );
    }
    scalar_statement(text, range, cursor, first, limits, check)
}

fn scalar_statement<E>(
    text: &[u8],
    range: Range<usize>,
    mut cursor: usize,
    first: &[u8],
    limits: BodyLimits,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleBodyStatement>, ModuleError<E>> {
    let local = if first.eq_ignore_ascii_case(b"Dim") {
        Some(ModuleLocalKind::Variable)
    } else if first.eq_ignore_ascii_case(b"Const") {
        Some(ModuleLocalKind::Constant)
    } else {
        None
    };
    if let Some(kind) = local {
        if let Some(statement) = local_declaration(text, range.clone(), cursor, kind, check)
            .map_err(ModuleError::Cancelled)?
        {
            if let ModuleBodyStatement::Declaration(declaration) = &statement
                && declaration
                    .value()
                    .is_some_and(|value| value.initializer_work_nodes() > limits.expression_nodes)
            {
                return Err(ModuleError::ExpressionLimit);
            }
            return Ok(Some(statement));
        }
        return if kind == ModuleLocalKind::Variable {
            local_variables(text, range, cursor, limits.records, check)
        } else {
            Ok(None)
        };
    }
    let explicit_let = first.eq_ignore_ascii_case(b"Let");
    if explicit_let {
        if !separator(text, &mut cursor, check).map_err(ModuleError::Cancelled)? {
            return Ok(None);
        }
    } else {
        cursor = 0;
    }
    let Some(destination) =
        simple_name(text, &mut cursor, check).map_err(ModuleError::Cancelled)?
    else {
        return Ok(None);
    };
    // Native statement-keyword paths can recover an apparent assignment at its
    // head. This guard stays contextual; other name positions are independent.
    if !explicit_let && text.get(destination.clone()).is_some_and(statement_keyword) {
        return Ok(None);
    }
    if !equals(text, &mut cursor, check).map_err(ModuleError::Cancelled)? {
        return Ok(None);
    }
    let expression_start = cursor;
    if let Some(statement) = scalar_assignment(
        text,
        range.clone(),
        cursor,
        destination.clone(),
        limits.arguments,
        check,
    )? {
        return Ok(Some(statement));
    }
    let Some((expression, inventory)) = module_expression::inventory(
        text,
        expression_start,
        &range,
        limits.expression_nodes,
        limits.arguments,
        check,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(ModuleBodyStatement::ExpressionAssignment(
        module_expression::ModuleExpressionAssignment {
            destination: absolute(&range, destination),
            expression,
            inventory,
            statement: range,
        },
    )))
}

fn scalar_assignment<E>(
    text: &[u8],
    range: Range<usize>,
    mut cursor: usize,
    destination: Range<usize>,
    maximum_arguments: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleBodyStatement>, ModuleError<E>> {
    let Some(value) =
        atomic_value(text, &mut cursor, true, check).map_err(ModuleError::Cancelled)?
    else {
        return Ok(None);
    };
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)
        .map_err(ModuleError::Cancelled)?;
    if value.kind == ModuleScalarValueKind::Name && text.get(cursor) == Some(&b'(') {
        // Line has a contextual value/assignment alias, not a proven callable
        // alias. Keep that bare token on the full grammar route; [Line] is a name.
        if text
            .get(value.span.clone())
            .is_some_and(|callee| callee.eq_ignore_ascii_case(b"Line"))
        {
            return Ok(None);
        }
        let arguments_start = cursor;
        cursor += 1;
        let Some(mut argument_values) =
            scalar_arguments(text, &mut cursor, maximum_arguments, check)?
        else {
            return Ok(None);
        };
        cursor += 1;
        let application_end = cursor;
        if !complete(text, &mut cursor, check).map_err(ModuleError::Cancelled)? {
            return Ok(None);
        }
        for argument in &mut argument_values {
            check().map_err(ModuleError::Cancelled)?;
            argument.rebase(&range);
        }
        return Ok(Some(ModuleBodyStatement::ApplicationAssignment(
            ModuleApplicationAssignment {
                destination: absolute(&range, destination),
                application: absolute(&range, value.span.start..application_end),
                callee: absolute(&range, value.span),
                arguments: absolute(&range, arguments_start..application_end),
                argument_values,
                statement: range,
            },
        )));
    }
    if !complete(text, &mut cursor, check).map_err(ModuleError::Cancelled)? {
        return Ok(None);
    }
    Ok(Some(ModuleBodyStatement::Assignment(
        ModuleScalarAssignment {
            destination: absolute(&range, destination),
            value: ModuleScalarValue {
                kind: value.kind,
                integer_suffix: value.integer_suffix,
                float_suffix: value.float_suffix,
                unary_minus: value.unary_minus.map(|minus| absolute(&range, minus)),
                span: absolute(&range, value.span),
            },
            statement: range,
        },
    )))
}

fn explicit_call<E>(
    text: &[u8],
    range: Range<usize>,
    mut cursor: usize,
    maximum_arguments: usize,
    maximum_expression_nodes: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleBodyStatement>, ModuleError<E>> {
    // The resolver recognizes an explicit Call only with an immediate space/tab
    // separator. Broader native extras must not fabricate explicit-call evidence.
    if !matches!(text.get(cursor), Some(b' ' | b'\t'))
        || !separator(text, &mut cursor, check).map_err(ModuleError::Cancelled)?
    {
        return Ok(None);
    }
    let Some(callee) = simple_name(text, &mut cursor, check).map_err(ModuleError::Cancelled)?
    else {
        return Ok(None);
    };
    // Bare Name has a contextual token in the native Call grammar. Retaining
    // that form on the native route avoids claiming complete equivalent facts.
    if text
        .get(callee.clone())
        .is_some_and(|spelling| spelling.eq_ignore_ascii_case(b"Name"))
    {
        return Ok(None);
    }
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)
        .map_err(ModuleError::Cancelled)?;
    if let Some(statement) = atomic_call(
        text,
        range.clone(),
        cursor,
        callee.clone(),
        maximum_arguments,
        check,
    )? {
        return Ok(Some(statement));
    }
    module_expression::expression_call(
        text,
        callee,
        range,
        maximum_expression_nodes,
        maximum_arguments,
        check,
    )
    .map(|call| call.map(ModuleBodyStatement::ExpressionCall))
}

fn atomic_call<E>(
    text: &[u8],
    range: Range<usize>,
    mut cursor: usize,
    callee: Range<usize>,
    maximum_arguments: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleBodyStatement>, ModuleError<E>> {
    let mut argument_values = Vec::new();
    let arguments = if text.get(cursor) == Some(&b'(') {
        let start = cursor;
        cursor += 1;
        let Some(values) = scalar_arguments(text, &mut cursor, maximum_arguments, check)? else {
            return Ok(None);
        };
        argument_values = values;
        cursor += 1;
        Some(absolute(&range, start..cursor))
    } else {
        None
    };
    if !complete(text, &mut cursor, check).map_err(ModuleError::Cancelled)? {
        return Ok(None);
    }
    for value in &mut argument_values {
        check().map_err(ModuleError::Cancelled)?;
        value.rebase(&range);
    }
    Ok(Some(ModuleBodyStatement::Call(ModuleCall {
        callee: absolute(&range, callee),
        arguments,
        argument_values,
        statement: range,
    })))
}

fn scalar_arguments<E>(
    text: &[u8],
    cursor: &mut usize,
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Vec<ModuleScalarValue>>, ModuleError<E>> {
    let mut values = Vec::new();
    *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)
        .map_err(ModuleError::Cancelled)?;
    if text.get(*cursor) == Some(&b')') {
        return Ok(Some(values));
    }
    loop {
        check().map_err(ModuleError::Cancelled)?;
        if values.len() >= maximum {
            return Err(ModuleError::ArgumentLimit);
        }
        let Some(value) =
            atomic_value(text, cursor, true, check).map_err(ModuleError::Cancelled)?
        else {
            return Ok(None);
        };
        reserve_arguments(&mut values, maximum, check)?;
        values.push(value);
        *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)
            .map_err(ModuleError::Cancelled)?;
        match text.get(*cursor) {
            Some(b')') => return Ok(Some(values)),
            Some(b',') => {
                *cursor += 1;
                *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)
                    .map_err(ModuleError::Cancelled)?;
                if matches!(text.get(*cursor), None | Some(b')' | b',')) {
                    return Ok(None);
                }
            }
            _ => return Ok(None),
        }
    }
}

fn reserve_arguments<E>(
    values: &mut Vec<ModuleScalarValue>,
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if values.len() >= maximum {
        return Err(ModuleError::ArgumentLimit);
    }
    if values.len() == values.capacity() {
        let additional = values.capacity().max(4).min(maximum - values.len());
        reserve_arguments_exact(values, additional, check)?;
    }
    Ok(())
}

fn reserve_arguments_exact<E>(
    values: &mut Vec<ModuleScalarValue>,
    additional: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    let result = values
        .try_reserve_exact(additional)
        .map_err(|_| ModuleError::AllocationFailed);
    check().map_err(ModuleError::Cancelled)?;
    result
}

fn local_variables<E>(
    text: &[u8],
    range: Range<usize>,
    mut cursor: usize,
    maximum_records: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleBodyStatement>, ModuleError<E>> {
    if !separator(text, &mut cursor, check).map_err(ModuleError::Cancelled)? {
        return Ok(None);
    }
    let mut variables = Vec::new();
    loop {
        check().map_err(ModuleError::Cancelled)?;
        let Some(syntax) = super::module::variable_declarator(text, &range, &mut cursor, check)
            .map_err(ModuleError::Cancelled)?
        else {
            return Ok(None);
        };
        if cursor != text.len() && text.get(cursor) != Some(&b',') {
            return Ok(None);
        }
        reserve_local_variables(&mut variables, maximum_records, check)?;
        variables.push(ModuleLocalVariable { syntax });
        if cursor == text.len() {
            return Ok(Some(ModuleBodyStatement::Variables(ModuleLocalVariables {
                statement: range,
                variables,
            })));
        }
        cursor += 1;
        cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)
            .map_err(ModuleError::Cancelled)?;
    }
}

fn reserve_local_variables<E>(
    variables: &mut Vec<ModuleLocalVariable>,
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if variables.len() >= maximum {
        return Err(ModuleError::StatementLimit);
    }
    if variables.len() == variables.capacity() {
        let additional = variables.capacity().max(4).min(maximum - variables.len());
        reserve_local_variables_exact(variables, additional, check)?;
    }
    Ok(())
}

fn reserve_local_variables_exact<E>(
    variables: &mut Vec<ModuleLocalVariable>,
    additional: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    variables
        .try_reserve_exact(additional)
        .map_err(|_| ModuleError::AllocationFailed)?;
    check().map_err(ModuleError::Cancelled)
}

fn local_declaration<E>(
    text: &[u8],
    range: Range<usize>,
    mut cursor: usize,
    kind: ModuleLocalKind,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleBodyStatement>, E> {
    if !separator(text, &mut cursor, check)? {
        return Ok(None);
    }
    let Some(name) = simple_name(text, &mut cursor, check)? else {
        return Ok(None);
    };
    if !separator(text, &mut cursor, check)?
        || !word(text, &mut cursor, check)?.eq_ignore_ascii_case(b"As")
        || !separator(text, &mut cursor, check)?
    {
        return Ok(None);
    }
    let type_start = cursor;
    if !intrinsic_type_name(word(text, &mut cursor, check)?) {
        return Ok(None);
    }
    let type_range = type_start..cursor;
    let value = if kind == ModuleLocalKind::Constant {
        if !equals(text, &mut cursor, check)? {
            return Ok(None);
        }
        let Some(mut value) = literal_value(text, &mut cursor, check)? else {
            return Ok(None);
        };
        check()?;
        value.rebase(&range);
        Some(value)
    } else {
        None
    };
    let declarator_end = cursor;
    if !complete(text, &mut cursor, check)? {
        return Ok(None);
    }
    Ok(Some(ModuleBodyStatement::Declaration(
        ModuleLocalDeclaration {
            kind,
            declarator: absolute(&range, name.start..declarator_end),
            name: absolute(&range, name),
            type_range: absolute(&range, type_range),
            value,
            statement: range,
        },
    )))
}

pub(super) fn literal_value<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleScalarValue>, E> {
    initializer_value(text, cursor, InitializerContext::Literal, check)
}

pub(super) fn parameter_default_value<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleScalarValue>, E> {
    initializer_value(text, cursor, InitializerContext::ParameterDefault, check)
}

#[derive(Clone, Copy)]
enum InitializerContext {
    Literal,
    ParameterDefault,
}

fn initializer_value<E>(
    text: &[u8],
    cursor: &mut usize,
    context: InitializerContext,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleScalarValue>, E> {
    // Only this declaration/default route retains one numeric unary minus.
    // Failed atom scans and cancellation must not advance the caller's cursor;
    // the enclosing grammar separately proves complete initializer boundaries.
    check()?;
    let start = *cursor;
    let mut end = start;
    let unary_minus = if text.get(end) == Some(&b'-') {
        end += 1;
        end += match context {
            InitializerContext::Literal => {
                logical_whitespace_len(text.get(end..).unwrap_or_default(), check)?
            }
            InitializerContext::ParameterDefault => {
                super::parameter::parameter_trivia_len(text.get(end..).unwrap_or_default(), check)?
            }
        };
        Some(start..start + 1)
    } else {
        None
    };
    let value = if unary_minus.is_some() {
        numeric_value(text, &mut end, check)?
    } else {
        atomic_value(
            text,
            &mut end,
            matches!(context, InitializerContext::ParameterDefault),
            check,
        )?
    };
    let Some(mut value) = value else {
        return Ok(None);
    };
    check()?;
    value.unary_minus = unary_minus;
    *cursor = end;
    Ok(Some(value))
}

pub(super) fn atomic_value<E>(
    text: &[u8],
    cursor: &mut usize,
    allow_name: bool,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleScalarValue>, E> {
    let start = *cursor;
    if let Some(value) = numeric_value(text, cursor, check)? {
        return Ok(Some(value));
    }
    let kind = if text.get(*cursor) == Some(&b'"') {
        *cursor += 1;
        let mut checkpoint = *cursor;
        loop {
            if *cursor >= checkpoint {
                check()?;
                checkpoint = cursor.saturating_add(512);
            }
            match text.get(*cursor) {
                Some(b'"') => {
                    *cursor += 1;
                    if text.get(*cursor) == Some(&b'"') {
                        *cursor += 1;
                    } else {
                        break;
                    }
                }
                None | Some(b'\r' | b'\n') => return Ok(None),
                Some(_) => *cursor += 1,
            }
        }
        ModuleScalarValueKind::String
    } else {
        let keyword = word(text, cursor, check)?;
        if keyword.eq_ignore_ascii_case(b"True") || keyword.eq_ignore_ascii_case(b"False") {
            ModuleScalarValueKind::Boolean
        } else if allow_name {
            *cursor = start;
            if simple_name(text, cursor, check)?.is_none() {
                return Ok(None);
            }
            ModuleScalarValueKind::Name
        } else {
            return Ok(None);
        }
    };
    Ok(Some(ModuleScalarValue {
        kind,
        integer_suffix: None,
        float_suffix: None,
        unary_minus: None,
        span: start..*cursor,
    }))
}

#[derive(Clone, Copy)]
enum IntegerRadix {
    Decimal,
    Hexadecimal,
    Octal,
}

impl IntegerRadix {
    fn admits(self, byte: u8) -> bool {
        match self {
            Self::Decimal => byte.is_ascii_digit(),
            Self::Hexadecimal => byte.is_ascii_hexdigit(),
            Self::Octal => matches!(byte, b'0'..=b'7'),
        }
    }

    fn kind(self) -> ModuleScalarValueKind {
        match self {
            Self::Decimal => ModuleScalarValueKind::UnsignedDecimal,
            Self::Hexadecimal => ModuleScalarValueKind::UnsignedHexadecimal,
            Self::Octal => ModuleScalarValueKind::UnsignedOctal,
        }
    }
}

fn integer_prefix(text: &[u8]) -> Option<(IntegerRadix, usize)> {
    // MS-VBAL 3.3.2 permits an omitted O/o marker after the octal ampersand.
    let (radix, prefix) = match text.first()? {
        byte if byte.is_ascii_digit() => (IntegerRadix::Decimal, 0),
        b'&' => match text.get(1) {
            Some(b'H' | b'h') => (IntegerRadix::Hexadecimal, 2),
            Some(b'O' | b'o') => (IntegerRadix::Octal, 2),
            _ => (IntegerRadix::Octal, 1),
        },
        _ => return None,
    };
    text.get(prefix)
        .is_some_and(|byte| radix.admits(*byte))
        .then_some((radix, prefix))
}

pub(super) fn radix_integer_prefix(text: &[u8]) -> bool {
    text.first() == Some(&b'&') && integer_prefix(text).is_some()
}

pub(super) fn numeric_value<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleScalarValue>, E> {
    // The enclosing grammar owns repeated-suffix and whole-statement closure; this scan
    // retains only the number token and never evaluates its magnitude or type.
    // Keep the caller's cursor unchanged until decimal INTEGER/FLOAT classification
    // is complete, so a malformed exponent cannot publish an integer prefix.
    let start = *cursor;
    let leading_point =
        text.get(start) == Some(&b'.') && text.get(start + 1).is_some_and(u8::is_ascii_digit);
    let (radix, prefix) = if leading_point {
        check()?;
        (IntegerRadix::Decimal, 1)
    } else if let Some(prefix) = integer_prefix(text.get(start..).unwrap_or_default()) {
        prefix
    } else {
        return Ok(None);
    };
    let mut end = start + prefix;
    numeric_digits(text, &mut end, radix, check)?;
    let mut is_float = leading_point;
    if matches!(radix, IntegerRadix::Decimal) {
        if !leading_point && text.get(end) == Some(&b'.') {
            check()?;
            end += 1;
            numeric_digits(text, &mut end, IntegerRadix::Decimal, check)?;
            is_float = true;
        }
        if matches!(text.get(end), Some(b'E' | b'e' | b'D' | b'd')) {
            check()?;
            end += 1;
            if matches!(text.get(end), Some(b'+' | b'-')) {
                check()?;
                end += 1;
            }
            let exponent_start = end;
            numeric_digits(text, &mut end, IntegerRadix::Decimal, check)?;
            if end == exponent_start {
                return Ok(None);
            }
            is_float = true;
        }
        is_float |= matches!(text.get(end), Some(b'!' | b'#' | b'@'));
    }
    let integer_suffix = if is_float {
        None
    } else {
        match text.get(end) {
            Some(b'%') => Some(ModuleIntegerSuffix::Percent),
            Some(b'&') => Some(ModuleIntegerSuffix::Ampersand),
            Some(b'^') => Some(ModuleIntegerSuffix::Caret),
            _ => None,
        }
    };
    let float_suffix = if is_float {
        match text.get(end) {
            Some(b'!') => Some(ModuleFloatSuffix::Exclamation),
            Some(b'#') => Some(ModuleFloatSuffix::Hash),
            Some(b'@') => Some(ModuleFloatSuffix::At),
            _ => None,
        }
    } else {
        None
    };
    if integer_suffix.is_some() || float_suffix.is_some() {
        check()?;
        end += 1;
    }
    *cursor = end;
    Ok(Some(ModuleScalarValue {
        kind: if is_float {
            ModuleScalarValueKind::UnsignedFloat
        } else {
            radix.kind()
        },
        integer_suffix,
        float_suffix,
        unary_minus: None,
        span: start..end,
    }))
}

fn numeric_digits<E>(
    text: &[u8],
    cursor: &mut usize,
    radix: IntegerRadix,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    while text.get(*cursor).is_some_and(|byte| radix.admits(*byte)) {
        if cursor.is_multiple_of(512) {
            check()?;
        }
        *cursor += 1;
    }
    Ok(())
}

pub(super) fn simple_name<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Range<usize>>, E> {
    let start = *cursor;
    let Some(spelling) = name(text, cursor, check)? else {
        return Ok(None);
    };
    if text.get(start) != Some(&b'[') && keyword_name(spelling) {
        return Ok(None);
    }
    Ok(Some(start..*cursor))
}

fn statement_keyword(name: &[u8]) -> bool {
    const KEYWORDS: &[u8] = b"Load Unload Input Print Close Reset Beep DefBool DefByte DefInt DefLng DefLngLng DefLngPtr DefCur DefSng DefDbl DefDate DefStr DefObj DefVar DefDec";
    KEYWORDS
        .split(u8::is_ascii_whitespace)
        .any(|keyword| name.eq_ignore_ascii_case(keyword))
}

fn keyword_name(name: &[u8]) -> bool {
    // Keyword-shaped names stay on the existing parser route. In particular Me
    // and literal keywords must not acquire fabricated ordinary value references.
    const KEYWORDS: &[u8] = b"Alias And As Attribute ByRef ByVal Call Case Const Declare Dim Do Each Else ElseIf Empty End Enum Eqv Erase Error Event Exit False For Friend Function Get GoSub GoTo If Imp Implements In Is Let Lib Like Lock Loop LSet Me Mod New Next Not Nothing Null On Open Optional Option Or ParamArray Preserve Private Property PtrSafe Public Put RaiseEvent Randomize ReDim Rem Resume Return RSet Seek Select Set Static Step Stop Sub Then To True Type TypeOf Unlock Until Wend While With WithEvents Write Xor";
    intrinsic_type_name(name)
        || KEYWORDS
            .split(u8::is_ascii_whitespace)
            .any(|keyword| name.eq_ignore_ascii_case(keyword))
}

pub(super) fn intrinsic_type_name(spelling: &[u8]) -> bool {
    [
        b"String".as_slice(),
        b"Boolean",
        b"Byte",
        b"Integer",
        b"Long",
        b"LongLong",
        b"LongPtr",
        b"Single",
        b"Double",
        b"Currency",
        b"Date",
        b"Variant",
        b"Object",
    ]
    .iter()
    .any(|keyword| spelling.eq_ignore_ascii_case(keyword))
}

fn equals<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
    if text.get(*cursor) != Some(&b'=') {
        return Ok(false);
    }
    *cursor += 1;
    *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
    Ok(true)
}

fn separator<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let length = logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
    *cursor += length;
    Ok(length != 0)
}

fn complete<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
    Ok(*cursor == text.len())
}

fn word<'a, E>(
    text: &'a [u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<&'a [u8], E> {
    let start = *cursor;
    while text
        .get(*cursor)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        if cursor.is_multiple_of(512) {
            check()?;
        }
        *cursor += 1;
    }
    Ok(text.get(start..*cursor).unwrap_or_default())
}

fn absolute(statement: &Range<usize>, local: Range<usize>) -> Range<usize> {
    statement.start + local.start..statement.start + local.end
}

pub(super) fn reserve<E>(
    statements: &mut Vec<ModuleBodyStatement>,
    maximum: usize,
) -> Result<(), ModuleError<E>> {
    if statements.len() >= maximum {
        return Err(ModuleError::StatementLimit);
    }
    if statements.len() == statements.capacity() {
        let additional = statements.capacity().max(1).min(maximum - statements.len());
        reserve_exact(statements, additional)?;
    }
    Ok(())
}

fn reserve_exact<E>(
    statements: &mut Vec<ModuleBodyStatement>,
    additional: usize,
) -> Result<(), ModuleError<E>> {
    statements
        .try_reserve_exact(additional)
        .map_err(|_| ModuleError::AllocationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &[u8]) -> Option<ModuleBodyStatement> {
        statement(text, 0..text.len(), text.len(), &mut || Ok::<(), ()>(())).unwrap()
    }

    #[test]
    fn unary_minus_initializers_keep_unsigned_tokens_suffixes_and_rebased_ranges() {
        for (base, kind) in [
            ("007", ModuleScalarValueKind::UnsignedDecimal),
            ("&H1E2", ModuleScalarValueKind::UnsignedHexadecimal),
            ("&hF", ModuleScalarValueKind::UnsignedHexadecimal),
            ("&O7", ModuleScalarValueKind::UnsignedOctal),
            ("&o1", ModuleScalarValueKind::UnsignedOctal),
            ("&77", ModuleScalarValueKind::UnsignedOctal),
            ("1.5", ModuleScalarValueKind::UnsignedFloat),
            ("5.", ModuleScalarValueKind::UnsignedFloat),
            (".5", ModuleScalarValueKind::UnsignedFloat),
            ("1E-2", ModuleScalarValueKind::UnsignedFloat),
            ("1e+2", ModuleScalarValueKind::UnsignedFloat),
            ("1D+2", ModuleScalarValueKind::UnsignedFloat),
            ("1d-2", ModuleScalarValueKind::UnsignedFloat),
        ] {
            let suffixes = if kind == ModuleScalarValueKind::UnsignedFloat {
                ["", "!", "#", "@"]
            } else {
                ["", "%", "&", "^"]
            };
            for suffix in suffixes {
                let token = format!("{base}{suffix}");
                let integer_suffix = match suffix {
                    "%" => Some(ModuleIntegerSuffix::Percent),
                    "&" => Some(ModuleIntegerSuffix::Ampersand),
                    "^" => Some(ModuleIntegerSuffix::Caret),
                    _ => None,
                };
                let float_suffix = match suffix {
                    "!" => Some(ModuleFloatSuffix::Exclamation),
                    "#" => Some(ModuleFloatSuffix::Hash),
                    "@" => Some(ModuleFloatSuffix::At),
                    _ => None,
                };
                for trivia in ["", " ", "\t", " _\n ", " _\r\n "] {
                    let prefix = "Const [Limit] As Double=";
                    let text = format!("{prefix}-{trivia}{token}");
                    let source = format!("prefix\n{text}\ntail");
                    let range = 7..7 + text.len();
                    let Some(ModuleBodyStatement::Declaration(declaration)) =
                        statement_with_expression_limits(
                            source.as_bytes(),
                            range.clone(),
                            0,
                            1,
                            &mut || Ok::<(), ()>(()),
                        )
                        .unwrap()
                    else {
                        panic!("complete numeric initializer expected: {text}")
                    };
                    let value = declaration.value().unwrap();
                    let minus_start = 7 + prefix.len();
                    let token_start = minus_start + 1 + trivia.len();
                    assert_eq!(declaration.statement(), range.clone());
                    assert_eq!(&source[declaration.name()], "[Limit]");
                    assert_eq!(&source[declaration.type_range()], "Double");
                    assert_eq!(value.span(), token_start..range.end);
                    assert_eq!(&source[value.span()], token);
                    assert_eq!(value.kind(), kind);
                    assert_eq!(value.integer_suffix(), integer_suffix);
                    assert_eq!(value.float_suffix(), float_suffix);
                    assert_eq!(value.unary_minus_span(), Some(minus_start..minus_start + 1));
                    assert_eq!(value.initializer_span(), minus_start..range.end);
                    assert_eq!(
                        &source[value.initializer_span()],
                        format!("-{trivia}{token}")
                    );
                    assert_eq!(value.initializer_work_nodes(), 1);
                    assert_eq!(declaration.declarator().end, range.end);
                    assert_eq!(
                        statement_with_expression_limits(
                            source.as_bytes(),
                            range,
                            0,
                            0,
                            &mut || Ok::<(), ()>(())
                        ),
                        Err(ModuleError::ExpressionLimit)
                    );
                }
                for signed in [false, true] {
                    let source = if signed {
                        format!("-{token}")
                    } else {
                        token.clone()
                    };
                    let mut cursor = 0;
                    let value =
                        parameter_default_value(source.as_bytes(), &mut cursor, &mut || {
                            Ok::<(), ()>(())
                        })
                        .unwrap()
                        .unwrap();
                    assert_eq!(cursor, source.len());
                    assert_eq!(&source[value.span()], token);
                    assert_eq!(value.kind(), kind);
                    assert_eq!(value.integer_suffix(), integer_suffix);
                    assert_eq!(value.float_suffix(), float_suffix);
                    assert_eq!(value.initializer_span(), 0..source.len());
                    assert_eq!(value.unary_minus_span(), signed.then_some(0..1));
                    assert_eq!(value.initializer_work_nodes(), usize::from(signed));
                }
            }
        }
    }

    #[test]
    fn unary_minus_initializer_scope_preserves_legacy_atoms_and_expression_operators() {
        for source in [
            "007", "&H1%", "&77^", "1E-2#", "\"text\"", "True", "[True]", "Seed",
        ] {
            let mut cursor = 0;
            let value =
                parameter_default_value(source.as_bytes(), &mut cursor, &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .unwrap();
            assert_eq!(value.span(), 0..source.len());
            assert_eq!(value.initializer_span(), value.span());
            assert_eq!(value.unary_minus_span(), None);
            assert_eq!(value.initializer_work_nodes(), 0);
        }
        for source in [
            "-",
            "--1",
            "-+1",
            "+1",
            "-True",
            "-\"text\"",
            "-Seed",
            "-[True]",
            "-(1)",
            "-Fetch(1)",
            "-1E",
            "-&H",
            "-\n1",
        ] {
            for context in [
                InitializerContext::Literal,
                InitializerContext::ParameterDefault,
            ] {
                let mut cursor = 0;
                assert_eq!(
                    initializer_value(
                        source.as_bytes(),
                        &mut cursor,
                        context,
                        &mut || Ok::<(), ()>(())
                    ),
                    Ok(None),
                    "{source}"
                );
                assert_eq!(cursor, 0);
            }
        }
        for tail in ["-1%%", "-1 extra", "-1+2", "-1.5#@", "-1E2^2", "-1,2"] {
            let source = format!("Const Limit As Double={tail}");
            assert!(read(source.as_bytes()).is_none(), "{source}");
        }
        let mut cursor = 0;
        assert_eq!(
            atomic_value(b"-1", &mut cursor, true, &mut || Ok::<(), ()>(())),
            Ok(None)
        );
        assert_eq!(cursor, 0);
        for source in ["sink=-1E-2#", "Call Target(-&H1%)"] {
            let statement = read(source.as_bytes()).unwrap();
            let (values, work) = match &statement {
                ModuleBodyStatement::ExpressionAssignment(assignment) => (
                    assignment.inventory().values(),
                    assignment.inventory().work_nodes(),
                ),
                ModuleBodyStatement::ExpressionCall(call) => {
                    (call.inventory().values(), call.inventory().work_nodes())
                }
                ModuleBodyStatement::Declaration(_)
                | ModuleBodyStatement::Variables(_)
                | ModuleBodyStatement::Assignment(_)
                | ModuleBodyStatement::ApplicationAssignment(_)
                | ModuleBodyStatement::Call(_) => {
                    panic!("unary operator must stay in a structured expression")
                }
            };
            assert_eq!(work, if source.starts_with("Call") { 4 } else { 2 });
            assert_eq!(values.len(), 1);
            assert_eq!(values[0].unary_minus_span(), None);
            assert_eq!(values[0].initializer_span(), values[0].span());
            assert_eq!(values[0].initializer_work_nodes(), 0);
        }
    }

    #[test]
    fn unary_minus_initializer_scans_are_transactional_at_every_callback() {
        for (initializer, context) in [
            (
                format!("-{}%", "9".repeat(1024)),
                InitializerContext::Literal,
            ),
            (
                format!("- _\r\n &H{}&", "F".repeat(1024)),
                InitializerContext::Literal,
            ),
            (
                format!("-.{}D-{}#", "5".repeat(1024), "2".repeat(1024)),
                InitializerContext::ParameterDefault,
            ),
            (
                format!("-{}1", " \x0cRem\x0c _\n ".repeat(128)),
                InitializerContext::ParameterDefault,
            ),
            (
                format!("\"{}\"", "x".repeat(1024)),
                InitializerContext::Literal,
            ),
            ("[Seed]".to_owned(), InitializerContext::ParameterDefault),
        ] {
            let source = format!("head{initializer}");
            let mut cursor = 4;
            let mut total = 0;
            let value = initializer_value(source.as_bytes(), &mut cursor, context, &mut || {
                total += 1;
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap();
            assert_eq!(cursor, source.len());
            assert_eq!(value.initializer_span(), 4..source.len());
            assert!(total > 1);
            for stop in 1..=total {
                let mut cursor = 4;
                let mut calls = 0;
                assert_eq!(
                    initializer_value(source.as_bytes(), &mut cursor, context, &mut || {
                        calls += 1;
                        if calls == stop { Err("stop") } else { Ok(()) }
                    }),
                    Err("stop")
                );
                assert_eq!(calls, stop);
                assert_eq!(cursor, 4);
            }
        }
        let source = format!("head-{}E+", "1".repeat(1024));
        let mut cursor = 4;
        let mut total = 0;
        assert_eq!(
            literal_value(source.as_bytes(), &mut cursor, &mut || {
                total += 1;
                Ok::<(), ()>(())
            }),
            Ok(None)
        );
        assert_eq!(cursor, 4);
        for stop in 1..=total {
            let mut cursor = 4;
            let mut calls = 0;
            assert_eq!(
                literal_value(source.as_bytes(), &mut cursor, &mut || {
                    calls += 1;
                    if calls == stop { Err("stop") } else { Ok(()) }
                }),
                Err("stop")
            );
            assert_eq!(calls, stop);
            assert_eq!(cursor, 4);
        }
    }

    #[test]
    fn float_atoms_preserve_spelling_suffix_metadata_and_legacy_atomic_routes() {
        for atom in [
            "1.0", ".5", "10.5", "7.5", "5.", "1E2", "1e+2", "1D-2", "1d2", "1.E+2", ".5d-2", "007",
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
                if let Some(suffix) = suffix {
                    assert_eq!(suffix.as_char().to_string(), marker);
                }
                for newline in ["\n", "\r\n"] {
                    for text in [
                        format!("Const [Limit] As Double = _{newline} {spelling}"),
                        format!("Let [sink] = _{newline} {spelling}"),
                        format!("Call [Target]( _{newline} {spelling})"),
                        format!("[sink]=[Read]( _{newline} {spelling})"),
                    ] {
                        let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
                        let range = 7..7 + text.len();
                        let parsed = statement_with_expression_limits(
                            &source,
                            range.clone(),
                            1,
                            0,
                            &mut || Ok::<(), ()>(()),
                        )
                        .unwrap()
                        .unwrap();
                        assert_eq!(parsed.statement(), range);
                        let value = match &parsed {
                            ModuleBodyStatement::Declaration(local) => local.value().unwrap(),
                            ModuleBodyStatement::Assignment(assignment) => assignment.value(),
                            ModuleBodyStatement::Call(call) => {
                                let legacy: &ModuleZeroArgumentCall = call;
                                assert_eq!(legacy.callee(), call.callee());
                                assert_eq!(call.argument_values().len(), 1);
                                &call.argument_values()[0]
                            }
                            ModuleBodyStatement::ApplicationAssignment(assignment) => {
                                &assignment.argument_values()[0]
                            }
                            _ => panic!("FLOAT atoms preserve legacy atomic records"),
                        };
                        assert_eq!(value.kind(), ModuleScalarValueKind::UnsignedFloat);
                        assert_eq!(value.integer_suffix(), None);
                        assert_eq!(value.float_suffix(), suffix);
                        assert_eq!(&source[value.span()], spelling.as_bytes());
                        assert_eq!(value.span().start, 7 + text.find(&spelling).unwrap());
                        assert_eq!(value.span().end, value.span().start + spelling.len());
                    }
                }
            }
        }
        for atom in [
            "007",
            "1%",
            "&H1&",
            "&77^",
            "\"value\"",
            "True",
            "False",
            "value",
            "[value]",
        ] {
            let text = format!("sink={atom}");
            let ModuleBodyStatement::Assignment(assignment) = read(text.as_bytes()).unwrap() else {
                panic!("legacy atom expected")
            };
            assert_ne!(
                assignment.value().kind(),
                ModuleScalarValueKind::UnsignedFloat
            );
            assert_eq!(assignment.value().float_suffix(), None);
            assert_eq!(&text[assignment.value().span()], atom);
        }
    }

    #[test]
    fn float_byte_boundaries_and_malformed_exponents_never_publish_integer_prefixes() {
        for byte in 0..=u8::MAX {
            for atom in [b"1.5".as_slice(), b".5", b"1E2", b"7!"] {
                let text = [b"sink=".as_slice(), atom, &[byte]].concat();
                let parsed = read(&text);
                let continuation =
                    atom != b"7!" && (byte.is_ascii_digit() || matches!(byte, b'!' | b'#' | b'@'));
                assert_eq!(
                    parsed.is_some(),
                    continuation || matches!(byte, b' ' | b'\t'),
                    "{atom:?} byte {byte}"
                );
                if let Some(ModuleBodyStatement::Assignment(assignment)) = parsed {
                    assert_eq!(
                        assignment.value().kind(),
                        ModuleScalarValueKind::UnsignedFloat
                    );
                    assert_eq!(assignment.value().integer_suffix(), None);
                    assert_eq!(assignment.value().span().start, 5);
                    assert_eq!(
                        assignment.value().span().end,
                        text.len() - usize::from(matches!(byte, b' ' | b'\t'))
                    );
                }
            }
        }
        for atom in [
            ".", "1e", "1E+", "1D-", ".e2", "1..2", "1.2eTrue", "1e\"x\"", "1e2.3", "1e2e3",
            "1e2d3", "1.5%", "&H1!", "&O7#", "&77@", "1.5!!", "1.5#@", "1.5@2", "1.5$", "1e+ 2",
            "1.5d- 2", "1 .5",
        ] {
            for text in [
                format!("sink={atom}"),
                format!("Call Target({atom})"),
                format!("Const Limit As Double={atom}"),
                format!("sink=Read({atom})"),
            ] {
                assert!(read(text.as_bytes()).is_none(), "{text}");
            }
        }
        for text in [b"1eTrue".as_slice(), b"1E+", b"1d-", b".5eFalse"] {
            let mut cursor = 0;
            assert!(
                numeric_value(text, &mut cursor, &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none()
            );
            assert_eq!(cursor, 0);
            assert!(
                atomic_value(text, &mut cursor, true, &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn float_exponent_signs_suffixes_and_array_endpoints_keep_exact_work_ceilings() {
        for newline in ["\n", "\r\n"] {
            let text = format!("Call Target(value + _{newline} 1E-2#, Inner(.5D+3@) * -2.5!)");
            let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
            let range = 7..7 + text.len();
            let ModuleBodyStatement::ExpressionCall(call) =
                statement_with_expression_limits(&source, range.clone(), 3, 11, &mut || {
                    Ok::<(), ()>(())
                })
                .unwrap()
                .unwrap()
            else {
                panic!("FLOAT expression Call expected")
            };
            assert_eq!(call.statement(), range);
            assert_eq!(call.inventory().work_nodes(), 11);
            assert_eq!(call.inventory().argument_count(), 3);
            assert_eq!(call.inventory().applications().len(), 2);
            assert_eq!(call.inventory().values().len(), 4);
            for (value, (spelling, suffix)) in call.inventory().values().iter().zip([
                ("value", None),
                ("1E-2#", Some(ModuleFloatSuffix::Hash)),
                (".5D+3@", Some(ModuleFloatSuffix::At)),
                ("2.5!", Some(ModuleFloatSuffix::Exclamation)),
            ]) {
                assert_eq!(&source[value.span()], spelling.as_bytes());
                assert_eq!(value.float_suffix(), suffix);
                assert_eq!(value.integer_suffix(), None);
            }
            for maximum in [0, 1, 10] {
                assert_eq!(
                    statement_with_expression_limits(
                        &source,
                        range.clone(),
                        3,
                        maximum,
                        &mut || Ok::<(), ()>(())
                    ),
                    Err(ModuleError::ExpressionLimit)
                );
            }
            for maximum in [0, 1, 2] {
                assert_eq!(
                    statement_with_expression_limits(
                        &source,
                        range.clone(),
                        maximum,
                        11,
                        &mut || Ok::<(), ()>(())
                    ),
                    Err(ModuleError::ArgumentLimit)
                );
            }
        }
        let text = b"Call Target(1.5,.5D+2,7!)";
        assert!(matches!(
            statement_with_expression_limits(text, 0..text.len(), 3, 0, &mut || Ok::<(), ()>(())),
            Ok(Some(ModuleBodyStatement::Call(_)))
        ));
        for maximum in [0, 1, 2] {
            assert_eq!(
                statement_with_expression_limits(text, 0..text.len(), maximum, 0, &mut || Ok::<
                    (),
                    (),
                >(
                    ()
                )),
                Err(ModuleError::ArgumentLimit)
            );
        }
        let text = b"Dim grid(-1.5 To+.5D+2, &H7%)";
        let limits = BodyLimits {
            arguments: 0,
            expression_nodes: 0,
            records: 1,
        };
        let ModuleBodyStatement::Variables(list) =
            statement_with_limits(text, 0..text.len(), limits, &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap()
        else {
            panic!("source-only mixed numeric array expected")
        };
        assert_eq!(
            &text[list.variables()[0].array_bounds().unwrap()],
            b"(-1.5 To+.5D+2, &H7%)"
        );
        assert_eq!(list.variables()[0].array_work_nodes(), 8);
        assert_eq!(
            statement_with_limits(
                text,
                0..text.len(),
                BodyLimits {
                    records: 0,
                    ..limits
                },
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::StatementLimit)
        );
    }

    #[test]
    fn float_long_significands_exponents_and_rebases_cancel_at_every_checkpoint() {
        let significand = "9".repeat(2048);
        let exponent = "8".repeat(2048);
        for text in [
            format!("Const Limit As Double={significand}.{significand}E+{exponent}#"),
            format!("Call Target(.{significand}d-{exponent}@,{significand}!)"),
            format!("Dim grid(-.{significand}D+{exponent} To+{significand}.{significand}#, 7!)"),
            format!("Call Target(.{significand}e-{exponent}! + 1, Inner(7#) * -2.5@)"),
        ] {
            let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
            let range = 7..7 + text.len();
            let mut checkpoints = 0;
            assert!(
                statement(&source, range.clone(), 3, &mut || {
                    checkpoints += 1;
                    Ok::<(), ()>(())
                })
                .unwrap()
                .is_some()
            );
            assert!(checkpoints > 8);
            for stop in 1..=checkpoints {
                let mut calls = 0;
                assert_eq!(
                    statement(&source, range.clone(), 3, &mut || {
                        calls += 1;
                        if calls == stop { Err("stop") } else { Ok(()) }
                    }),
                    Err(ModuleError::Cancelled("stop")),
                    "checkpoint {stop}"
                );
                assert_eq!(calls, stop);
            }
        }
        let invalid = format!("{significand}.{significand}e+");
        let mut cursor = 0;
        let mut checkpoints = 0;
        assert!(
            numeric_value(invalid.as_bytes(), &mut cursor, &mut || {
                checkpoints += 1;
                Ok::<(), ()>(())
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(cursor, 0);
        assert!(checkpoints > 4);
        for stop in 1..=checkpoints {
            let mut cursor = 0;
            let mut calls = 0;
            assert_eq!(
                numeric_value(invalid.as_bytes(), &mut cursor, &mut || {
                    calls += 1;
                    if calls == stop { Err("stop") } else { Ok(()) }
                }),
                Err("stop")
            );
            assert_eq!(cursor, 0);
            assert_eq!(calls, stop);
        }
    }

    #[test]
    fn integer_suffix_atoms_preserve_exact_kind_metadata_and_legacy_routes() {
        for (atom, kind) in [
            ("1", ModuleScalarValueKind::UnsignedDecimal),
            ("001", ModuleScalarValueKind::UnsignedDecimal),
            ("&H0A", ModuleScalarValueKind::UnsignedHexadecimal),
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
                let spelling = format!("{atom}{}", suffix.as_char());
                for newline in ["\n", "\r\n"] {
                    for text in [
                        format!("Const [Limit] As Long = _{newline} {spelling}"),
                        format!("Let [sink] = _{newline} {spelling}"),
                        format!("Call [Target]( _{newline} {spelling})"),
                        format!("[sink]=[Read]( _{newline} {spelling})"),
                    ] {
                        let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
                        let range = 7..7 + text.len();
                        let parsed = statement_with_expression_limits(
                            &source,
                            range.clone(),
                            1,
                            0,
                            &mut || Ok::<(), ()>(()),
                        )
                        .unwrap()
                        .unwrap();
                        assert_eq!(parsed.statement(), range);
                        let value = match &parsed {
                            ModuleBodyStatement::Declaration(declaration) => {
                                declaration.value().unwrap()
                            }
                            ModuleBodyStatement::Assignment(assignment) => assignment.value(),
                            ModuleBodyStatement::Call(call) => {
                                let legacy: &ModuleZeroArgumentCall = call;
                                assert_eq!(legacy.callee(), call.callee());
                                assert_eq!(call.argument_values().len(), 1);
                                &call.argument_values()[0]
                            }
                            ModuleBodyStatement::ApplicationAssignment(assignment) => {
                                assert_eq!(assignment.argument_values().len(), 1);
                                &assignment.argument_values()[0]
                            }
                            _ => panic!("typed integer must preserve the old atomic route"),
                        };
                        assert_eq!(value.kind(), kind);
                        assert_eq!(value.integer_suffix(), Some(suffix));
                        assert_eq!(&source[value.span()], spelling.as_bytes());
                        assert_eq!(char::from(source[value.span().end - 1]), suffix.as_char());
                    }
                }
            }
        }
        for atom in [
            "007",
            "&H1",
            "&77",
            "\"value\"",
            "True",
            "False",
            "value",
            "[value]",
        ] {
            let text = format!("sink={atom}");
            let ModuleBodyStatement::Assignment(assignment) = read(text.as_bytes()).unwrap() else {
                panic!("unsuffixed atomic assignment expected")
            };
            assert_eq!(assignment.value().integer_suffix(), None);
            assert_eq!(&text[assignment.value().span()], atom);
        }
    }

    #[test]
    fn integer_suffix_byte_boundaries_retain_one_marker_and_reject_adjacent_tails() {
        for byte in 0..=u8::MAX {
            let suffix = match byte {
                b'%' => Some(ModuleIntegerSuffix::Percent),
                b'&' => Some(ModuleIntegerSuffix::Ampersand),
                b'^' => Some(ModuleIntegerSuffix::Caret),
                _ => None,
            };
            for (atom, digit) in [
                (b"1".as_slice(), byte.is_ascii_digit()),
                (b"&H1".as_slice(), byte.is_ascii_hexdigit()),
                (b"&O7".as_slice(), matches!(byte, b'0'..=b'7')),
                (b"&77".as_slice(), matches!(byte, b'0'..=b'7')),
            ] {
                let source = [b"sink=".as_slice(), atom, &[byte]].concat();
                let parsed = read(&source);
                let decimal_float = atom == b"1" && matches!(byte, b'.' | b'!' | b'#' | b'@');
                assert_eq!(
                    parsed.is_some(),
                    digit || suffix.is_some() || decimal_float || matches!(byte, b' ' | b'\t'),
                    "{atom:?}, byte {byte}"
                );
                if let Some(ModuleBodyStatement::Assignment(assignment)) = parsed {
                    assert_eq!(assignment.value().integer_suffix(), suffix);
                    assert_eq!(
                        assignment.value().kind() == ModuleScalarValueKind::UnsignedFloat,
                        decimal_float
                    );
                    assert_eq!(assignment.value().span().start, 5);
                    assert_eq!(
                        assignment.value().span().end,
                        source.len() - usize::from(matches!(byte, b' ' | b'\t'))
                    );
                }
                for marker in *b"%&^" {
                    let source = [b"sink=".as_slice(), atom, &[marker, byte]].concat();
                    assert_eq!(
                        read(&source).is_some(),
                        matches!(byte, b' ' | b'\t'),
                        "{atom:?}, marker {marker}, adjacent byte {byte}"
                    );
                }
            }
        }
        for suffix in [
            "%%", "&&", "^^", "%&", "&^", "^%", "%!", "&#", "^@", "%$", "%2", "&2", "^2",
        ] {
            for atom in ["1", "&H1", "&O7", "&77"] {
                for text in [
                    format!("sink={atom}{suffix}"),
                    format!("Call Target({atom}{suffix})"),
                    format!("Const Limit As Long={atom}{suffix}"),
                    format!("sink=Read({atom}{suffix})"),
                ] {
                    assert!(read(text.as_bytes()).is_none(), "{text}");
                }
            }
        }
        for atom in [
            "True%",
            "False&",
            "\"text\"^",
            "value%",
            "[value]&",
            "&H%",
            "&O&",
            "&^",
        ] {
            let text = format!("sink={atom}");
            assert!(read(text.as_bytes()).is_none(), "{text}");
        }
    }

    #[test]
    fn integer_suffix_expression_and_array_work_preserve_existing_exact_ceilings() {
        for newline in ["\n", "\r\n"] {
            let text =
                format!("Call Target(value + _{newline} 1%, Inner(&77&) * - _{newline} &hF^)");
            let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
            let range = 7..7 + text.len();
            let ModuleBodyStatement::ExpressionCall(call) =
                statement_with_expression_limits(&source, range.clone(), 3, 11, &mut || {
                    Ok::<(), ()>(())
                })
                .unwrap()
                .unwrap()
            else {
                panic!("typed integer expression arguments expected")
            };
            assert_eq!(call.statement(), range);
            assert_eq!(&source[call.callee()], b"Target");
            assert_eq!(call.inventory().work_nodes(), 11);
            assert_eq!(call.inventory().argument_count(), 3);
            assert_eq!(call.inventory().applications()[0].callee(), call.callee());
            assert_eq!(call.inventory().values().len(), 4);
            for (value, (spelling, suffix)) in call.inventory().values().iter().zip([
                ("value", None),
                ("1%", Some(ModuleIntegerSuffix::Percent)),
                ("&77&", Some(ModuleIntegerSuffix::Ampersand)),
                ("&hF^", Some(ModuleIntegerSuffix::Caret)),
            ]) {
                assert_eq!(&source[value.span()], spelling.as_bytes());
                assert_eq!(value.integer_suffix(), suffix);
            }
            for maximum in [0, 1, 10] {
                assert_eq!(
                    statement_with_expression_limits(
                        &source,
                        range.clone(),
                        3,
                        maximum,
                        &mut || Ok::<(), ()>(())
                    ),
                    Err(ModuleError::ExpressionLimit)
                );
            }
            for maximum in [0, 1, 2] {
                assert_eq!(
                    statement_with_expression_limits(
                        &source,
                        range.clone(),
                        maximum,
                        11,
                        &mut || Ok::<(), ()>(())
                    ),
                    Err(ModuleError::ArgumentLimit)
                );
            }
        }
        let text = b"Call Target(1%,&H2&,&o7^)";
        assert!(matches!(
            statement_with_expression_limits(text, 0..text.len(), 3, 0, &mut || Ok::<(), ()>(())),
            Ok(Some(ModuleBodyStatement::Call(_)))
        ));
        for maximum in [0, 1, 2] {
            assert_eq!(
                statement_with_expression_limits(text, 0..text.len(), maximum, 0, &mut || Ok::<
                    (),
                    (),
                >(
                    ()
                )),
                Err(ModuleError::ArgumentLimit)
            );
        }
        let text = b"Dim grid(-1% To+&H2&, &o7^)";
        let limits = BodyLimits {
            arguments: 0,
            expression_nodes: 0,
            records: 1,
        };
        let ModuleBodyStatement::Variables(list) =
            statement_with_limits(text, 0..text.len(), limits, &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap()
        else {
            panic!("typed integer local bounds expected")
        };
        assert_eq!(list.variables()[0].array_bounds(), Some(8..text.len()));
        assert_eq!(list.variables()[0].array_work_nodes(), 8);
        assert_eq!(
            statement_with_limits(
                text,
                0..text.len(),
                BodyLimits {
                    records: 0,
                    ..limits
                },
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::StatementLimit)
        );
    }

    #[test]
    fn integer_suffix_scan_and_rebase_cancellation_remain_typed_at_every_checkpoint() {
        for text in [
            format!("Const Limit As Long={}^", "9".repeat(2048)),
            format!(
                "Call Target(&H{}%,&{}&)",
                "F".repeat(2048),
                "7".repeat(2048)
            ),
            format!(
                "Dim grid(-&H{}% To+&O{}&, 1^)",
                "F".repeat(2048),
                "7".repeat(2048)
            ),
            format!(
                "Call Target(&H{}% + 1, Inner(&{}&) * -2^)",
                "F".repeat(2048),
                "7".repeat(2048)
            ),
        ] {
            let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
            let range = 7..7 + text.len();
            let mut checkpoints = 0;
            assert!(
                statement(&source, range.clone(), 3, &mut || {
                    checkpoints += 1;
                    Ok::<(), ()>(())
                })
                .unwrap()
                .is_some()
            );
            assert!(checkpoints > 8);
            for stop in 1..=checkpoints {
                let mut calls = 0;
                assert_eq!(
                    statement(&source, range.clone(), 3, &mut || {
                        calls += 1;
                        if calls == stop { Err("stop") } else { Ok(()) }
                    }),
                    Err(ModuleError::Cancelled("stop")),
                    "checkpoint {stop}"
                );
                assert_eq!(calls, stop);
            }
        }
    }

    #[test]
    fn integer_atoms_retain_all_radix_spellings_and_decimal_compatibility() {
        for (spelling, kind) in [
            ("00019", ModuleScalarValueKind::UnsignedDecimal),
            ("&H0A", ModuleScalarValueKind::UnsignedHexadecimal),
            ("&hFf", ModuleScalarValueKind::UnsignedHexadecimal),
            ("&H1E2", ModuleScalarValueKind::UnsignedHexadecimal),
            ("&O007", ModuleScalarValueKind::UnsignedOctal),
            ("&o71", ModuleScalarValueKind::UnsignedOctal),
            ("&77", ModuleScalarValueKind::UnsignedOctal),
            ("&0", ModuleScalarValueKind::UnsignedOctal),
        ] {
            let source = format!("prefix:{spelling},tail");
            let mut cursor = 7;
            let value = atomic_value(source.as_bytes(), &mut cursor, false, &mut || {
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap();
            assert_eq!(value.kind(), kind);
            assert_eq!(value.span(), 7..7 + spelling.len());
            assert_eq!(&source[value.span()], spelling);
            assert_eq!(cursor, 7 + spelling.len());
            assert_eq!(source.as_bytes()[cursor], b',');
        }
        for invalid in ["&", "&H", "&hG", "&O", "&o8", "&8", "& 7", "&H 1"] {
            let mut cursor = 0;
            assert_eq!(
                numeric_value(invalid.as_bytes(), &mut cursor, &mut || Ok::<(), ()>(())),
                Ok(None),
                "{invalid}"
            );
            assert_eq!(cursor, 0);
        }
    }

    #[test]
    fn radix_atoms_keep_atomic_declaration_assignment_call_and_application_routes() {
        for (spelling, kind) in [
            ("&H10", ModuleScalarValueKind::UnsignedHexadecimal),
            ("&hA", ModuleScalarValueKind::UnsignedHexadecimal),
            ("&O7", ModuleScalarValueKind::UnsignedOctal),
            ("&o7", ModuleScalarValueKind::UnsignedOctal),
            ("&77", ModuleScalarValueKind::UnsignedOctal),
        ] {
            for text in [
                format!("Const [Limit] As Long={spelling}"),
                format!("Let [sink] = {spelling}"),
                format!("Call [Target]({spelling})"),
                format!("[sink]=[Read]({spelling})"),
            ] {
                let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
                let range = 7..7 + text.len();
                let parsed =
                    statement_with_expression_limits(&source, range.clone(), 1, 0, &mut || {
                        Ok::<(), ()>(())
                    })
                    .unwrap()
                    .unwrap();
                assert_eq!(parsed.statement(), range);
                let value = match &parsed {
                    ModuleBodyStatement::Declaration(declaration) => {
                        assert_eq!(declaration.kind(), ModuleLocalKind::Constant);
                        assert_eq!(&source[declaration.type_range()], b"Long");
                        declaration.value().unwrap()
                    }
                    ModuleBodyStatement::Assignment(assignment) => assignment.value(),
                    ModuleBodyStatement::Call(call) => {
                        let legacy: &ModuleZeroArgumentCall = call;
                        assert_eq!(legacy.callee(), call.callee());
                        assert_eq!(call.argument_values().len(), 1);
                        &call.argument_values()[0]
                    }
                    ModuleBodyStatement::ApplicationAssignment(assignment) => {
                        assert_eq!(assignment.argument_values().len(), 1);
                        &assignment.argument_values()[0]
                    }
                    _ => panic!("radix atom must preserve the legacy atomic route"),
                };
                assert_eq!(value.kind(), kind);
                assert_eq!(&source[value.span()], spelling.as_bytes());
                assert_eq!(
                    value.span().end,
                    parsed.statement().end - usize::from(text.ends_with(')'))
                );
            }
        }
    }

    #[test]
    fn signed_and_composed_radix_values_use_complete_expression_records() {
        for (text, expected, work, arguments) in [
            ("count = -&H1", vec!["&H1"], 2, 0),
            ("count=Read(0,+&H1,2)", vec!["0", "&H1", "2"], 6, 3),
            ("count=Read(1)+&H2", vec!["1", "&H2"], 5, 1),
            ("Call Target(&H10+2)", vec!["&H10", "2"], 5, 1),
            ("Call Target(&77 * -&o2)", vec!["&77", "&o2"], 6, 1),
        ] {
            let parsed = statement_with_expression_limits(
                text.as_bytes(),
                0..text.len(),
                arguments,
                work,
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .unwrap();
            let inventory = match &parsed {
                ModuleBodyStatement::ExpressionAssignment(assignment) => assignment.inventory(),
                ModuleBodyStatement::ExpressionCall(call) => {
                    assert_eq!(&text[call.callee()], "Target");
                    assert_eq!(call.inventory().applications()[0].callee(), call.callee());
                    call.inventory()
                }
                _ => panic!("signed or composed radix expression expected"),
            };
            assert_eq!(parsed.statement(), 0..text.len());
            assert_eq!(inventory.work_nodes(), work);
            assert_eq!(inventory.argument_count(), arguments);
            assert_eq!(
                inventory
                    .values()
                    .iter()
                    .map(|value| &text[value.span()])
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                statement_with_expression_limits(
                    text.as_bytes(),
                    0..text.len(),
                    arguments,
                    work - 1,
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::ExpressionLimit),
                "{text}"
            );
        }
    }

    #[test]
    fn radix_atom_byte_alphabets_and_typed_tails_remain_closed() {
        for byte in 0..=u8::MAX {
            for (prefix, admits) in [
                (b"count=&H".as_slice(), byte.is_ascii_hexdigit()),
                (b"count=&h".as_slice(), byte.is_ascii_hexdigit()),
                (b"count=&O".as_slice(), matches!(byte, b'0'..=b'7')),
                (b"count=&o".as_slice(), matches!(byte, b'0'..=b'7')),
                (b"count=&".as_slice(), matches!(byte, b'0'..=b'7')),
            ] {
                let source = [prefix, &[byte]].concat();
                assert_eq!(read(&source).is_some(), admits, "{prefix:?}, byte {byte}");
            }
            for (atom, admits_digit) in [
                (b"&H1".as_slice(), byte.is_ascii_hexdigit()),
                (b"&hA".as_slice(), byte.is_ascii_hexdigit()),
                (b"&O7".as_slice(), matches!(byte, b'0'..=b'7')),
                (b"&o7".as_slice(), matches!(byte, b'0'..=b'7')),
                (b"&77".as_slice(), matches!(byte, b'0'..=b'7')),
            ] {
                let source = [b"count=".as_slice(), atom, &[byte]].concat();
                assert_eq!(
                    read(&source).is_some(),
                    admits_digit || matches!(byte, b' ' | b'\t' | b'%' | b'&' | b'^'),
                    "atom {atom:?}, tail byte {byte}"
                );
                let source = [b"Call Target(".as_slice(), atom, &[byte], b")".as_slice()].concat();
                assert_eq!(
                    read(&source).is_some(),
                    admits_digit || matches!(byte, b' ' | b'\t' | b'%' | b'&' | b'^'),
                    "call atom {atom:?}, tail byte {byte}"
                );
            }
        }
        for atom in [
            "&", "&H", "&hG", "&O", "&o8", "&8", "&78", "&H1G", "&O7a", "&H1.0", "&O7.0", "&77E2",
            "& H1", "&O 7", "&H_1", "&H1more",
        ] {
            for text in [
                format!("count={atom}"),
                format!("Const Limit As Long={atom}"),
                format!("Call Target(0,{atom},2)"),
                format!("count=Read(0,{atom},2)"),
            ] {
                assert!(read(text.as_bytes()).is_none(), "{text}");
            }
        }
        for atom in ["&H1", "&hF", "&O7", "&o1", "&77"] {
            for suffix in ['!', '#', '@', '$'] {
                for text in [
                    format!("count={atom}{suffix}"),
                    format!("count={atom}{suffix}2"),
                    format!("Const Limit As Long={atom}{suffix}"),
                    format!("Call Target({atom}{suffix}+2)"),
                    format!("count=Read({atom}{suffix})"),
                ] {
                    assert!(read(text.as_bytes()).is_none(), "{text}");
                }
            }
        }
        for text in ["Const Limit As Long=--&H1", "Const Limit As Long=+&77"] {
            assert!(
                read(text.as_bytes()).is_none(),
                "unsupported literal initializer: {text}"
            );
        }
    }

    #[test]
    fn radix_body_scans_and_expression_rebasing_cancel_at_every_checkpoint() {
        for text in [
            format!("Const Limit As Long=&H{}", "F".repeat(2048)),
            format!("Call Target(&O{}, &{})", "7".repeat(2048), "1".repeat(2048)),
            format!(
                "Dim grid(-&H{} To +&O{}, &77)",
                "F".repeat(2048),
                "7".repeat(2048)
            ),
            format!(
                "Call Target(&H{}+1, Inner(&{}) * -&o2)",
                "F".repeat(2048),
                "7".repeat(2048)
            ),
        ] {
            let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
            let range = 7..7 + text.len();
            let mut checkpoints = 0;
            let admitted = statement(&source, range.clone(), 3, &mut || {
                checkpoints += 1;
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap();
            assert_eq!(admitted.statement(), range);
            assert!(checkpoints > 8);
            for stop in 1..=checkpoints {
                let mut calls = 0;
                assert_eq!(
                    statement(&source, range.clone(), 3, &mut || {
                        calls += 1;
                        if calls == stop { Err("stop") } else { Ok(()) }
                    }),
                    Err(ModuleError::Cancelled("stop")),
                    "checkpoint {stop}"
                );
                assert_eq!(calls, stop);
            }
        }
    }

    #[test]
    fn local_variable_lists_preserve_independent_rebased_types_names_and_array_ranges() {
        for newline in ["\n", "\r\n"] {
            let bounds = format!("( - _{newline} 1 To +2, _{newline} 3)");
            let text = format!(
                "dIm _{newline} [first], second _{newline} As _{newline} Long, [grid]{bounds} _{newline} As Variant, pending()"
            );
            let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
            let range = 7..7 + text.len();
            let ModuleBodyStatement::Variables(list) = statement_with_limits(
                &source,
                range.clone(),
                BodyLimits {
                    arguments: 0,
                    expression_nodes: 0,
                    records: 4,
                },
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .unwrap() else {
                panic!("complete local variable list expected");
            };
            assert_eq!(list.statement(), range);
            assert_eq!(list.variables().len(), 4);
            let first = &list.variables()[0];
            assert_eq!(&source[first.name()], b"[first]");
            assert_eq!(first.declarator(), first.name());
            assert!(first.type_range().is_none());
            assert!(first.array_bounds().is_none());
            assert_eq!(first.array_work_nodes(), 0);
            let second = &list.variables()[1];
            assert_eq!(&source[second.name()], b"second");
            assert_eq!(&source[second.type_range().unwrap()], b"Long");
            assert_eq!(
                &source[second.declarator()],
                format!("second _{newline} As _{newline} Long").as_bytes()
            );
            let grid = &list.variables()[2];
            assert_eq!(&source[grid.name()], b"[grid]");
            assert_eq!(&source[grid.array_bounds().unwrap()], bounds.as_bytes());
            assert_eq!(&source[grid.type_range().unwrap()], b"Variant");
            assert_eq!(grid.array_work_nodes(), 8);
            let pending = &list.variables()[3];
            assert_eq!(&source[pending.name()], b"pending");
            assert_eq!(&source[pending.array_bounds().unwrap()], b"()");
            assert_eq!(&source[pending.declarator()], b"pending()");
            assert!(pending.type_range().is_none());
            assert_eq!(pending.array_work_nodes(), 1);
            let mut end = list.statement().start;
            for variable in list.variables() {
                assert!(end <= variable.declarator().start);
                assert!(variable.declarator().start < variable.declarator().end);
                assert!(variable.declarator().end <= list.statement().end);
                assert!(variable.declarator().start <= variable.name().start);
                assert!(variable.name().end <= variable.declarator().end);
                end = variable.declarator().end;
            }
        }
    }

    #[test]
    fn local_variables_admit_untyped_groups_and_arrays_without_changing_typed_scalar_api() {
        for ty in [
            "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
            "Double", "Currency", "Date", "Variant", "Object",
        ] {
            let scalar = format!("Dim [value] As {ty}");
            let ModuleBodyStatement::Declaration(declaration) = read(scalar.as_bytes()).unwrap()
            else {
                panic!("single typed scalar retains original API in {scalar}");
            };
            assert_eq!(declaration.kind(), ModuleLocalKind::Variable);
            assert_eq!(&scalar[declaration.type_range()], ty);
            assert!(declaration.value().is_none());
            let text = format!("Dim first, [value] As {ty}, grid(-1 To +2, 3) As {ty}");
            let ModuleBodyStatement::Variables(list) = read(text.as_bytes()).unwrap() else {
                panic!("complete grouped variables expected in {text}");
            };
            assert!(list.variables()[0].type_range().is_none());
            assert_eq!(&text[list.variables()[1].type_range().unwrap()], ty);
            assert_eq!(&text[list.variables()[2].type_range().unwrap()], ty);
            assert_eq!(list.variables()[2].array_work_nodes(), 8);
        }
        for (text, count) in [
            ("Dim count", 1),
            ("Dim x, y As Long", 2),
            ("Dim values() As Long", 1),
            ("Dim [True], [Me], [Rem], [Sub], [Name], [Line]", 6),
            ("Dim first As Long,second As Long", 2),
            ("Dim items(1,2), pending(), [items]", 3),
        ] {
            let ModuleBodyStatement::Variables(list) = read(text.as_bytes()).unwrap() else {
                panic!("newly supported variable syntax {text}");
            };
            assert_eq!(list.statement(), 0..text.len());
            assert_eq!(list.variables().len(), count);
        }
        let text = b"Const Limit As Long=7";
        let ModuleBodyStatement::Declaration(declaration) = read(text).unwrap() else {
            panic!("Const keeps original declaration API");
        };
        assert_eq!(declaration.kind(), ModuleLocalKind::Constant);
        assert_eq!(&text[declaration.value().unwrap().span()], b"7");
    }

    #[test]
    fn local_array_designators_match_existing_complete_numeric_syntax_without_evaluation() {
        for (bounds, work) in [
            ("()", 1),
            ("(1)", 3),
            ("(-1)", 4),
            ("(+1)", 4),
            ("(1 To 2)", 4),
            ("(-1 To +2)", 6),
            ("(-1 To +2,3)", 8),
            ("(1,2,3)", 7),
            ("(3 To 1)", 4),
            ("(0To1)", 4),
        ] {
            let text = format!("Dim value{bounds}");
            let ModuleBodyStatement::Variables(list) = read(text.as_bytes()).unwrap() else {
                panic!("complete numeric bounds in {text}");
            };
            let variable = &list.variables()[0];
            assert_eq!(&text[variable.array_bounds().unwrap()], bounds);
            assert_eq!(variable.array_work_nodes(), work);
            assert!(variable.type_range().is_none());
        }
        let text = format!("Dim value({} To +{})", "9".repeat(4096), "8".repeat(4096));
        let ModuleBodyStatement::Variables(list) = read(text.as_bytes()).unwrap() else {
            panic!("numeric magnitudes are source syntax, not allocation sizes");
        };
        assert_eq!(list.variables()[0].array_work_nodes(), 5);
    }

    #[test]
    fn local_variable_retention_limits_and_allocation_cancellation_are_typed() {
        let source = b"Dim first, second(), third(-1 To +2) As Long";
        let limits = BodyLimits {
            arguments: 0,
            expression_nodes: 0,
            records: 3,
        };
        let ModuleBodyStatement::Variables(list) =
            statement_with_limits(source, 0..source.len(), limits, &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap()
        else {
            panic!("exact record credit expected");
        };
        assert_eq!(list.variables().len(), 3);
        for records in [0, 1, 2] {
            assert_eq!(
                statement_with_limits(
                    source,
                    0..source.len(),
                    BodyLimits { records, ..limits },
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::StatementLimit),
                "retained local record ceiling {records}"
            );
        }
        let mut empty = Vec::new();
        assert_eq!(
            reserve_local_variables(&mut empty, 0, &mut || Ok::<(), ()>(())),
            Err(ModuleError::StatementLimit)
        );
        assert_eq!(empty.capacity(), 0);
        assert_eq!(
            reserve_local_variables_exact(&mut empty, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert!(empty.is_empty());
        for stop in 1..=2 {
            let mut variables = Vec::new();
            let mut calls = 0;
            assert_eq!(
                reserve_local_variables_exact(&mut variables, 1, &mut || {
                    calls += 1;
                    if calls == stop { Err("stop") } else { Ok(()) }
                }),
                Err(ModuleError::Cancelled("stop"))
            );
            assert!(variables.is_empty());
            assert_eq!(calls, stop);
            if stop == 1 {
                assert_eq!(variables.capacity(), 0);
            }
        }
    }

    #[test]
    fn local_variable_late_malformed_members_never_return_a_partial_list() {
        for tail in [
            "",
            ",last",
            "last,",
            "last,,other",
            "last As",
            "last As Product",
            "last As New Object",
            "last As String * 8",
            "last As Long()",
            "last$",
            "[last]$",
            "last=1",
            "last As Long=1",
            "holder.last",
            "last(upper)",
            "last(1+2)",
            "last(,1)",
            "last(1,)",
            "last(1,,2)",
            "last(1 To)",
            "last(1",
            "last(1)(2)",
            "last(1) As",
            "last(1) As Product",
            "last(1)=1",
            "[open",
            "[]",
            "[last name]",
            "é",
            "Dim last",
            "Public last",
            "Private last",
            "Static last",
            "WithEvents last As Object",
            "Empty",
            "Me",
            "last extra",
            "last\0",
            "last\r extra",
            "last\nother",
        ] {
            let source = format!("Dim first, valid(-1 To +2,3) As Long, {tail}");
            assert!(read(source.as_bytes()).is_none(), "{tail:?}");
        }
        for source in [
            "Dim",
            "Dim()",
            "Dim[first]",
            "Dim first:Dim second",
            "Dim first ' tail",
            "Dim first Rem tail",
            "Private first",
            "Public first As Long",
            "Static first As Long",
            "Const first=1",
            "Const first As Long=1, second As Long=2",
        ] {
            assert!(read(source.as_bytes()).is_none(), "{source}");
        }
    }

    #[test]
    fn large_local_variable_lists_keep_order_and_exact_bounded_sibling_retention() {
        let count = 256;
        let text = format!(
            "Dim {}",
            (0..count)
                .map(|index| format!("value{index}({index})"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let limits = BodyLimits {
            arguments: 0,
            expression_nodes: 0,
            records: count,
        };
        let ModuleBodyStatement::Variables(list) =
            statement_with_limits(text.as_bytes(), 0..text.len(), limits, &mut || {
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap()
        else {
            panic!("large complete local list expected");
        };
        assert_eq!(list.variables().len(), count);
        for (index, variable) in list.variables().iter().enumerate() {
            assert_eq!(&text[variable.name()], format!("value{index}"));
            assert_eq!(
                &text[variable.array_bounds().unwrap()],
                format!("({index})")
            );
            assert_eq!(variable.array_work_nodes(), 3);
        }
        assert_eq!(
            statement_with_limits(
                text.as_bytes(),
                0..text.len(),
                BodyLimits {
                    records: count - 1,
                    ..limits
                },
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::StatementLimit)
        );
    }

    #[test]
    fn local_variable_all_byte_boundaries_preserve_complete_lists_and_record_ranges() {
        for byte in 0..=u8::MAX {
            for (prefix, suffix, expected) in [
                (
                    b"Dim".as_slice(),
                    b"first".as_slice(),
                    matches!(byte, b' ' | b'\t'),
                ),
                (
                    b"Dim first,",
                    b"second",
                    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b' ' | b'\t'),
                ),
                (b"Dim first, [second]", b"", matches!(byte, b' ' | b'\t')),
                (
                    b"Dim first, grid(",
                    b"2)",
                    byte.is_ascii_digit()
                        || matches!(byte, b'+' | b'-' | b'&' | b'.' | b' ' | b'\t'),
                ),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let parsed = read(&source);
                assert_eq!(
                    parsed.is_some(),
                    expected,
                    "{prefix:?} byte {byte} {suffix:?}"
                );
                if let Some(ModuleBodyStatement::Variables(list)) = parsed {
                    assert_eq!(list.statement(), 0..source.len());
                    for variable in list.variables() {
                        assert!(variable.declarator().start < variable.declarator().end);
                        assert!(variable.declarator().end <= source.len());
                        assert!(variable.name().end <= variable.declarator().end);
                    }
                }
            }
        }
    }

    #[test]
    fn local_variable_scan_parse_reserve_and_rebase_cancel_at_every_callback() {
        let text = format!(
            "Dim _\r\n [{}], _\n grid(- _\r\n {} To +{}, _\n 3) _\r\n As Long, _\n pending()",
            "identifier".repeat(128),
            "9".repeat(1024),
            "8".repeat(1024)
        );
        let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
        let range = 7..7 + text.len();
        let limits = BodyLimits {
            arguments: 0,
            expression_nodes: 0,
            records: 3,
        };
        let mut checkpoints = 0;
        let admitted = statement_with_limits(&source, range.clone(), limits, &mut || {
            checkpoints += 1;
            Ok::<(), ()>(())
        })
        .unwrap()
        .unwrap();
        assert!(matches!(admitted, ModuleBodyStatement::Variables(_)));
        for stop in 1..=checkpoints {
            let mut calls = 0;
            assert_eq!(
                statement_with_limits(&source, range.clone(), limits, &mut || {
                    calls += 1;
                    if calls == stop { Err("stop") } else { Ok(()) }
                }),
                Err(ModuleError::Cancelled("stop")),
                "checkpoint {stop}"
            );
            assert_eq!(calls, stop);
        }
    }

    #[test]
    fn local_declarations_retain_statement_declarator_name_type_and_value() {
        for (source, kind, name, ty, value) in [
            (
                "Dim [count] As Long",
                ModuleLocalKind::Variable,
                "[count]",
                "Long",
                None,
            ),
            (
                "Const Limit As Long = 007",
                ModuleLocalKind::Constant,
                "Limit",
                "Long",
                Some(("007", ModuleScalarValueKind::UnsignedDecimal)),
            ),
            (
                "Const TextValue As String=\"a\"\"b\"",
                ModuleLocalKind::Constant,
                "TextValue",
                "String",
                Some(("\"a\"\"b\"", ModuleScalarValueKind::String)),
            ),
            (
                "Const Enabled As Boolean = tRuE",
                ModuleLocalKind::Constant,
                "Enabled",
                "Boolean",
                Some(("tRuE", ModuleScalarValueKind::Boolean)),
            ),
            (
                "Dim _\r\n [count] _\n As _\n Long",
                ModuleLocalKind::Variable,
                "[count]",
                "Long",
                None,
            ),
        ] {
            let ModuleBodyStatement::Declaration(declaration) = read(source.as_bytes()).unwrap()
            else {
                panic!("declaration expected")
            };
            assert_eq!(declaration.kind(), kind);
            assert_eq!(declaration.statement(), 0..source.len());
            assert_eq!(&source[declaration.name()], name);
            assert_eq!(&source[declaration.type_range()], ty);
            assert!(source[declaration.declarator()].starts_with(name));
            assert_eq!(declaration.declarator().end, source.len());
            assert_eq!(
                declaration
                    .value()
                    .map(|value| (&source[value.span()], value.kind())),
                value
            );
        }
    }

    #[test]
    fn assignments_admit_only_complete_atoms_and_keep_bracketed_keywords_as_names() {
        for (source, destination, spelling, kind) in [
            (
                "count=7",
                "count",
                "7",
                ModuleScalarValueKind::UnsignedDecimal,
            ),
            (
                "Let [count] = Limit",
                "[count]",
                "Limit",
                ModuleScalarValueKind::Name,
            ),
            (
                "count = False",
                "count",
                "False",
                ModuleScalarValueKind::Boolean,
            ),
            ("count=\"\"", "count", "\"\"", ModuleScalarValueKind::String),
            ("[Me]=[True]", "[Me]", "[True]", ModuleScalarValueKind::Name),
            ("Name=Name", "Name", "Name", ModuleScalarValueKind::Name),
            ("Line=Line", "Line", "Line", ModuleScalarValueKind::Name),
            (
                "[Load]=1",
                "[Load]",
                "1",
                ModuleScalarValueKind::UnsignedDecimal,
            ),
            (
                "[Unload]=1",
                "[Unload]",
                "1",
                ModuleScalarValueKind::UnsignedDecimal,
            ),
            (
                "Let Load=1",
                "Load",
                "1",
                ModuleScalarValueKind::UnsignedDecimal,
            ),
            (
                "lEt UnLoAd=1",
                "UnLoAd",
                "1",
                ModuleScalarValueKind::UnsignedDecimal,
            ),
            ("value=Load", "value", "Load", ModuleScalarValueKind::Name),
            (
                "Let _\n count _\r\n = _\n Limit",
                "count",
                "Limit",
                ModuleScalarValueKind::Name,
            ),
        ] {
            let ModuleBodyStatement::Assignment(assignment) = read(source.as_bytes()).unwrap()
            else {
                panic!("assignment expected")
            };
            assert_eq!(&source[assignment.destination()], destination);
            assert_eq!(&source[assignment.value().span()], spelling);
            assert_eq!(assignment.value().kind(), kind);
            assert_eq!(assignment.statement(), 0..source.len());
        }
    }

    #[test]
    fn explicit_calls_retain_exact_callee_and_empty_parentheses() {
        for (source, callee, arguments) in [
            ("Call Finish", "Finish", None),
            ("Call Finish()", "Finish", Some("()")),
            ("cAlL\t[Finish] \t( \t)", "[Finish]", Some("( \t)")),
            ("Call [Me]()", "[Me]", Some("()")),
            ("Call [True]", "[True]", None),
            ("Call [Rem]()", "[Rem]", Some("()")),
            ("Call [Sub]()", "[Sub]", Some("()")),
            ("Call Line", "Line", None),
            ("Call [Name]()", "[Name]", Some("()")),
            ("cAlL [nAmE]", "[nAmE]", None),
            ("Call _Target()", "_Target", Some("()")),
            ("Call Load()", "Load", Some("()")),
            ("Call _\r\n [End] _\n ( _\r\n )", "[End]", Some("( _\r\n )")),
        ] {
            let ModuleBodyStatement::Call(call) = read(source.as_bytes()).unwrap() else {
                panic!("explicit call expected")
            };
            assert_eq!(call.statement(), 0..source.len());
            assert_eq!(&source[call.callee()], callee);
            assert_eq!(call.arguments().map(|range| &source[range]), arguments);
            assert!(call.argument_values().is_empty());
            let legacy: &ModuleZeroArgumentCall = &call;
            assert_eq!(legacy.callee(), call.callee());
            assert!(call.callee().start > call.statement().start);
            assert!(call.callee().end <= call.statement().end);
        }
    }

    #[test]
    fn explicit_scalar_calls_retain_all_atoms_and_legacy_source_spelling() {
        for (text, expected) in [
            (
                "Call Finish(1)",
                vec![("1", ModuleScalarValueKind::UnsignedDecimal)],
            ),
            (
                "Call Finish(value)",
                vec![("value", ModuleScalarValueKind::Name)],
            ),
            (
                "Call Finish(\"\")",
                vec![("\"\"", ModuleScalarValueKind::String)],
            ),
            (
                "Call [Name](001,\"\",tRuE,False,value,[True],[Me])",
                vec![
                    ("001", ModuleScalarValueKind::UnsignedDecimal),
                    ("\"\"", ModuleScalarValueKind::String),
                    ("tRuE", ModuleScalarValueKind::Boolean),
                    ("False", ModuleScalarValueKind::Boolean),
                    ("value", ModuleScalarValueKind::Name),
                    ("[True]", ModuleScalarValueKind::Name),
                    ("[Me]", ModuleScalarValueKind::Name),
                ],
            ),
            (
                "cAlL\t[Finish] _\r\n ( _\n 7 _\r\n, _\n \"a,:='Rem End Sub \"\"q\"\"\" ,[Me] _\r\n )",
                vec![
                    ("7", ModuleScalarValueKind::UnsignedDecimal),
                    (
                        "\"a,:='Rem End Sub \"\"q\"\"\"",
                        ModuleScalarValueKind::String,
                    ),
                    ("[Me]", ModuleScalarValueKind::Name),
                ],
            ),
        ] {
            let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
            let range = 7..7 + text.len();
            let ModuleBodyStatement::Call(call) =
                statement(&source, range.clone(), expected.len(), &mut || {
                    Ok::<(), ()>(())
                })
                .unwrap()
                .unwrap()
            else {
                panic!("complete scalar call expected");
            };
            assert_eq!(call.statement(), range);
            assert_eq!(call.argument_values().len(), expected.len());
            let parentheses = call.arguments().unwrap();
            assert_eq!(source[parentheses.start], b'(');
            assert_eq!(source[parentheses.end - 1], b')');
            let mut previous_end = parentheses.start + 1;
            for (value, (spelling, kind)) in call.argument_values().iter().zip(expected) {
                assert_eq!(&source[value.span()], spelling.as_bytes());
                assert_eq!(value.kind(), kind);
                assert!(previous_end <= value.span().start);
                assert!(value.span().end < parentheses.end);
                previous_end = value.span().end;
            }
            let legacy: &ModuleZeroArgumentCall = &call;
            assert_eq!(legacy.argument_values(), call.argument_values());
        }
        let arguments = (0..256)
            .map(|number| number.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let text = format!("Call Finish({arguments})");
        let ModuleBodyStatement::Call(call) = read(text.as_bytes()).unwrap() else {
            panic!("many scalar arguments expected");
        };
        assert_eq!(call.argument_values().len(), 256);
        for (index, value) in call.argument_values().iter().enumerate() {
            assert_eq!(&text[value.span()], index.to_string());
            assert_eq!(value.kind(), ModuleScalarValueKind::UnsignedDecimal);
        }
    }

    #[test]
    fn incomplete_or_unsupported_call_arguments_never_return_a_partial_list() {
        for unsupported in [
            "",
            " ",
            ",",
            "value:=1",
            "[value]:=1",
            "ByVal value",
            "ByRef value",
            "()",
            "target.member",
            ".member",
            "target!member",
            "1.0e",
            ".5d",
            "1E+",
            "10.5e-",
            "7.5d+",
            "#1/1/2000#",
            "1&&",
            "value$",
            "value!",
            "True$",
            "Nothing",
            "Null",
            "Empty",
            "Me",
            "Optional value",
            "ParamArray",
            "\"open",
            "\"continued _\n",
            "\"physical\nline\"",
            "\"closed\" extra",
            "[open",
            "[value name]",
            "[]",
            "[value$]",
            "1more",
            "True False",
            "1;2",
            "1\n",
            "1 ' inline\n",
        ] {
            let source = format!("Call Finish(0,{unsupported},2)");
            assert!(read(source.as_bytes()).is_none(), "{unsupported:?}");
        }
        for source in [
            "Call Finish(1,)",
            "Call Finish(,1)",
            "Call Finish(1,,2)",
            "Call Finish(1, _\n )",
            "Call Finish(1",
            "Call Finish(1) extra",
            "Call Finish(1)()",
            "Call Finish(1).member",
            "Call Finish(1)=2",
            "Call Finish 1",
            "Call Finish 1,2",
            "Call Finish value,other",
            "Call Name(1)",
            "cAlL nAmE(value)",
            "Call Finish(1) Rem tail",
        ] {
            assert!(read(source.as_bytes()).is_none(), "{source}");
        }
    }

    #[test]
    fn expression_calls_admit_former_atom_boundaries_with_exact_outer_argument_ranges() {
        for (source, expected, work, arguments) in [
            ("Call Finish(1+2)", vec!["1+2"], 5, 1),
            ("Call Finish(1 - 2)", vec!["1 - 2"], 5, 1),
            ("Call Finish(True And False)", vec!["True And False"], 5, 1),
            ("Call Finish(Not True)", vec!["Not True"], 4, 1),
            ("Call Finish(-1)", vec!["-1"], 4, 1),
            ("Call Finish(+1)", vec!["+1"], 4, 1),
            ("Call Finish((1))", vec!["(1)"], 4, 1),
            ("Call Finish(target())", vec!["target()"], 4, 1),
            ("Call Finish(0,(1),2)", vec!["0", "(1)", "2"], 6, 3),
            ("Call Finish(Read(1))", vec!["Read(1)"], 5, 2),
            ("Call Line(1+2)", vec!["1+2"], 5, 1),
            ("Call [Name](1+2)", vec!["1+2"], 5, 1),
        ] {
            let ModuleBodyStatement::ExpressionCall(call) = read(source.as_bytes()).unwrap() else {
                panic!("complete expression Call expected in {source}");
            };
            assert_eq!(call.statement(), 0..source.len());
            assert_eq!(call.inventory().work_nodes(), work);
            assert_eq!(call.inventory().argument_count(), arguments);
            assert_eq!(
                call.argument_expressions()
                    .iter()
                    .map(|range| &source[range.clone()])
                    .collect::<Vec<_>>(),
                expected
            );
            let outer = &call.inventory().applications()[0];
            assert_eq!(outer.callee(), call.callee());
            assert_eq!(outer.arguments(), call.arguments());
            assert_eq!(outer.argument_expressions(), call.argument_expressions());
            assert_eq!(outer.application(), call.callee().start..source.len());
        }
    }

    #[test]
    fn expression_calls_preserve_rebased_ranges_nested_records_and_logical_trivia() {
        for newline in ["\n", "\r\n"] {
            let parentheses = format!("(value _{newline} + 1, [Inner] _{newline} (other) * -2)");
            let text = format!("cAlL\t_{newline} [Target] _{newline} {parentheses}");
            let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
            let range = 7..7 + text.len();
            let ModuleBodyStatement::ExpressionCall(call) =
                statement_with_expression_limits(&source, range.clone(), 3, 11, &mut || {
                    Ok::<(), ()>(())
                })
                .unwrap()
                .unwrap()
            else {
                panic!("complete nested expression Call expected");
            };
            assert_eq!(call.statement(), range);
            assert_eq!(&source[call.callee()], b"[Target]");
            assert_eq!(&source[call.arguments()], parentheses.as_bytes());
            assert_eq!(call.inventory().work_nodes(), 11);
            assert_eq!(call.inventory().argument_count(), 3);
            assert_eq!(
                call.inventory()
                    .values()
                    .iter()
                    .map(|value| &source[value.span()])
                    .collect::<Vec<_>>(),
                [b"value".as_slice(), b"1", b"other", b"2"]
            );
            let applications = call.inventory().applications();
            assert_eq!(applications.len(), 2);
            assert_eq!(&source[applications[0].callee()], b"[Target]");
            assert_eq!(&source[applications[1].callee()], b"[Inner]");
            assert_eq!(call.argument_expressions().len(), 2);
            assert_eq!(
                &source[call.argument_expressions()[0].clone()],
                format!("value _{newline} + 1").as_bytes()
            );
            assert_eq!(
                &source[call.argument_expressions()[1].clone()],
                format!("[Inner] _{newline} (other) * -2").as_bytes()
            );
            assert_eq!(
                &source[applications[1].argument_expressions()[0].clone()],
                b"other"
            );
            for application in applications {
                assert!(call.callee().start <= application.application().start);
                assert!(application.application().end <= call.arguments().end);
                for argument in application.argument_expressions() {
                    assert!(application.arguments().start < argument.start);
                    assert!(argument.start < argument.end);
                    assert!(argument.end < application.arguments().end);
                }
            }
        }
    }

    #[test]
    fn expression_calls_require_the_exact_outer_application_not_an_expression_prefix() {
        for source in [
            "Call Target(1)+2",
            "Call Target(1) = 2",
            "Call (Target(1))",
            "Call -Target(1)",
            "Call Not Target(1)",
            "Call Target(1)(2)",
            "Call Target(1+2)()",
            "Call Target(1+2).member",
            "Call Target(1+2) trailing",
            "Call Target(1+)",
            "Call Target((1+2)",
            "Call Target(1+2))",
            "Call Target(1+2,)",
            "Call Target(,1+2)",
            "Call Target(1+2,,3)",
            "Call Target(value:=1+2)",
            "Call Target(ByVal value+1)",
            "Call Target(ByRef value+1)",
            "Call Target(Inner(value:=1))",
            "Call Target(Inner(1,))",
            "Call Target(Inner(1).member)",
            "Call Target(Line(1)+2)",
            "Call Name(1+2)",
            "Call Target 1+2",
            "Call Target value+1,2",
        ] {
            assert!(read(source.as_bytes()).is_none(), "{source}");
        }
    }

    #[test]
    fn expression_calls_retain_every_operator_without_claiming_evaluation_or_binding() {
        for operator in [
            "^", "*", "/", "\\", "Mod", "+", "-", "&", "=", "<>", "<", "<=", ">", ">=", "Is",
            "Like", "And", "Or", "Xor", "Eqv", "Imp",
        ] {
            let expression = format!("left {operator} right");
            let source = format!("Call Target({expression})");
            let ModuleBodyStatement::ExpressionCall(call) = read(source.as_bytes()).unwrap() else {
                panic!("complete operator argument expected in {source}");
            };
            assert_eq!(call.inventory().work_nodes(), 5);
            assert_eq!(call.inventory().argument_count(), 1);
            assert_eq!(call.inventory().applications().len(), 1);
            assert_eq!(&source[call.argument_expressions()[0].clone()], expression);
            assert_eq!(
                call.inventory()
                    .values()
                    .iter()
                    .map(|value| &source[value.span()])
                    .collect::<Vec<_>>(),
                ["left", "right"]
            );
        }
        for source in [
            "Call Target(a<b<c)",
            "Call Target(a<b Xor c>d Eqv e=f Imp g=h)",
            "Call Target(Not a=b And c=d)",
            "Call Target(a ^ -b ^ c)",
        ] {
            let ModuleBodyStatement::ExpressionCall(call) = read(source.as_bytes()).unwrap() else {
                panic!("normative structured argument expected in {source}");
            };
            assert_eq!(call.inventory().applications().len(), 1);
            assert_eq!(call.argument_expressions().len(), 1);
            assert_eq!(call.argument_expressions()[0].end, source.len() - 1);
        }
    }

    #[test]
    fn expression_call_limits_are_exact_and_never_retry_after_a_typed_failure() {
        let source = b"Call Target(value + 1, Inner(other) * -2)";
        let range = 0..source.len();
        let admitted =
            statement_with_expression_limits(
                source,
                range.clone(),
                3,
                11,
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .unwrap();
        assert!(matches!(admitted, ModuleBodyStatement::ExpressionCall(_)));
        for maximum in [0, 1, 2] {
            assert_eq!(
                statement_with_expression_limits(source, range.clone(), maximum, 11, &mut || {
                    Ok::<(), ()>(())
                }),
                Err(ModuleError::ArgumentLimit),
                "argument ceiling {maximum}"
            );
        }
        for maximum in [0, 1, 2, 5, 10] {
            assert_eq!(
                statement_with_expression_limits(source, range.clone(), 3, maximum, &mut || {
                    Ok::<(), ()>(())
                }),
                Err(ModuleError::ExpressionLimit),
                "expression ceiling {maximum}"
            );
        }
        for source in [
            b"Call Target".as_slice(),
            b"Call Target()",
            b"Call Target(1,value,True)",
        ] {
            let ModuleBodyStatement::Call(call) =
                statement_with_expression_limits(source, 0..source.len(), 3, 0, &mut || {
                    Ok::<(), ()>(())
                })
                .unwrap()
                .unwrap()
            else {
                panic!("legacy atomic Call must not consume expression work");
            };
            let legacy: &ModuleZeroArgumentCall = &call;
            assert_eq!(legacy.argument_values(), call.argument_values());
        }
    }

    #[test]
    fn expression_call_separator_callee_and_closed_statement_byte_boundaries_are_exact() {
        for byte in 0..=u8::MAX {
            for (prefix, suffix, expected) in [
                (
                    b"Call".as_slice(),
                    b"Target(1+2)".as_slice(),
                    matches!(byte, b' ' | b'\t'),
                ),
                (
                    b"Call Target",
                    b"(1+2)",
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b' ' | b'\t'),
                ),
                (b"Call Target(1+2)", b"", matches!(byte, b' ' | b'\t')),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let parsed = read(&source);
                assert_eq!(
                    parsed.is_some(),
                    expected,
                    "{prefix:?} byte {byte} {suffix:?}"
                );
                if let Some(ModuleBodyStatement::ExpressionCall(call)) = parsed {
                    assert_eq!(call.statement(), 0..source.len());
                    assert_eq!(call.inventory().work_nodes(), 5);
                    assert_eq!(call.inventory().argument_count(), 1);
                    assert_eq!(&source[call.argument_expressions()[0].clone()], b"1+2");
                    assert_eq!(call.inventory().applications().len(), 1);
                }
            }
        }
    }

    #[test]
    fn expression_call_adjacent_type_suffixes_and_richer_literals_fail_closed() {
        for (atom, suffixes) in [("2!", b"%!#@$".as_slice()), ("value", b"%&^!#@$")] {
            for suffix in suffixes {
                for tail in ["", "3", "other", "(3)", " + 3"] {
                    let source = format!(
                        "Call Target(Inner({atom}{}{tail}) + 1)",
                        char::from(*suffix)
                    );
                    assert!(read(source.as_bytes()).is_none(), "{source}");
                }
            }
        }
        for expression in [
            "2^3",
            "a&b",
            "1.0e+ +2",
            "1E+ +3",
            "10.5d+ +2",
            "#1/1/2000#",
            "Me+1",
            "Nothing",
            "target.member",
        ] {
            let source = format!("Call Target({expression})");
            assert!(read(source.as_bytes()).is_none(), "{source}");
        }
        for expression in [
            "2 ^3",
            "a & b",
            "[a]&[b]",
            "(a)^b",
            "Inner(1)^2",
            "\"a\"&\"b\"",
            "True&False",
        ] {
            let source = format!("Call Target({expression})");
            assert!(
                matches!(
                    read(source.as_bytes()),
                    Some(ModuleBodyStatement::ExpressionCall(_))
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn expression_call_cancellation_is_typed_at_every_probe_parse_and_rebase_checkpoint() {
        let text = b"Call [Target] _\r\n ((value + 007), Inner(\"a\"\"b\" & [True]))";
        let source = [b"prefix\n".as_slice(), text, b"\ntail"].concat();
        let range = 7..7 + text.len();
        let mut checkpoints = 0;
        let admitted = statement(&source, range.clone(), 3, &mut || {
            checkpoints += 1;
            Ok::<(), ()>(())
        })
        .unwrap()
        .unwrap();
        assert!(matches!(admitted, ModuleBodyStatement::ExpressionCall(_)));
        for cancelled_at in 1..=checkpoints {
            let mut calls = 0;
            assert_eq!(
                statement(&source, range.clone(), 3, &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err(ModuleError::Cancelled("stop")),
                "checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
    }

    #[test]
    fn scalar_call_argument_delimiters_and_all_byte_boundaries_are_exact() {
        for byte in 0..=u8::MAX {
            let tail = [b"Call Finish(1".as_slice(), &[byte], b")"].concat();
            let result = read(&tail);
            assert_eq!(
                result.is_some(),
                byte.is_ascii_digit()
                    || matches!(
                        byte,
                        b' ' | b'\t' | b'%' | b'&' | b'^' | b'.' | b'!' | b'#' | b'@'
                    ),
                "argument tail byte {byte}"
            );
            if let Some(ModuleBodyStatement::Call(call)) = result {
                assert_eq!(call.argument_values().len(), 1);
                assert_eq!(
                    call.argument_values()[0].kind(),
                    if matches!(byte, b'.' | b'!' | b'#' | b'@') {
                        ModuleScalarValueKind::UnsignedFloat
                    } else {
                        ModuleScalarValueKind::UnsignedDecimal
                    }
                );
                assert!(call.argument_values()[0].span().end < call.arguments().unwrap().end);
            }
            let next = [b"Call Finish(1,".as_slice(), &[byte], b"2)"].concat();
            let result = read(&next);
            assert_eq!(
                result.is_some(),
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'_' | b' ' | b'\t' | b'+' | b'-' | b'&' | b'.'),
                "argument prefix byte {byte}"
            );
            if let Some(statement) = result {
                match statement {
                    ModuleBodyStatement::Call(call) => {
                        assert_eq!(call.argument_values().len(), 2);
                        for value in call.argument_values() {
                            assert!(call.arguments().unwrap().start < value.span().start);
                            assert!(value.span().end < call.arguments().unwrap().end);
                            assert!(value.span().start <= value.span().end);
                        }
                    }
                    ModuleBodyStatement::ExpressionCall(call) => {
                        assert!(matches!(byte, b'+' | b'-'));
                        assert_eq!(call.statement(), 0..next.len());
                        assert_eq!(call.argument_expressions().len(), 2);
                        assert_eq!(call.inventory().argument_count(), 2);
                        assert_eq!(call.inventory().work_nodes(), 5);
                        assert_eq!(&next[call.argument_expressions()[1].clone()], &[byte, b'2']);
                    }
                    _ => panic!("complete explicit Call expected"),
                }
            }
            let suffix = [b"Call Finish(1)".as_slice(), &[byte]].concat();
            assert_eq!(
                read(&suffix).is_some(),
                matches!(byte, b' ' | b'\t'),
                "call suffix byte {byte}"
            );
        }
    }

    #[test]
    fn argument_storage_limits_overflow_and_reservation_cancellation_are_typed() {
        let mut values = Vec::new();
        assert_eq!(
            reserve_arguments(&mut values, 0, &mut || Ok::<(), ()>(())),
            Err(ModuleError::ArgumentLimit)
        );
        assert_eq!(values.capacity(), 0);
        reserve_arguments(&mut values, 1, &mut || Ok::<(), ()>(())).unwrap();
        values.push(ModuleScalarValue {
            kind: ModuleScalarValueKind::UnsignedDecimal,
            integer_suffix: None,
            float_suffix: None,
            unary_minus: None,
            span: 0..1,
        });
        let snapshot = values.clone();
        assert_eq!(
            reserve_arguments(&mut values, 1, &mut || Ok::<(), ()>(())),
            Err(ModuleError::ArgumentLimit)
        );
        assert_eq!(
            reserve_arguments_exact(&mut values, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert_eq!(values, snapshot);
        for cancelled_at in [1, 2] {
            let mut empty = Vec::new();
            let mut calls = 0;
            assert_eq!(
                reserve_arguments_exact(&mut empty, 1, &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err(ModuleError::Cancelled("stop"))
            );
            assert_eq!(calls, cancelled_at);
            assert!(empty.is_empty());
            if cancelled_at == 1 {
                assert_eq!(empty.capacity(), 0);
            }
        }
        for (source, maximum, expected_count) in [
            ("Call Finish", 0, 0),
            ("Call Finish()", 0, 0),
            ("Call Finish(1,value,True)", 3, 3),
        ] {
            let ModuleBodyStatement::Call(call) =
                statement(source.as_bytes(), 0..source.len(), maximum, &mut || {
                    Ok::<(), ()>(())
                })
                .unwrap()
                .unwrap()
            else {
                panic!("complete call expected");
            };
            assert_eq!(call.argument_values().len(), expected_count);
        }
        let source = b"Call Finish(1,value,True)";
        for maximum in [0, 1, 2] {
            assert_eq!(
                statement(source, 0..source.len(), maximum, &mut || Ok::<(), ()>(())),
                Err(ModuleError::ArgumentLimit)
            );
        }
    }

    #[test]
    fn scalar_call_cancellation_is_preserved_at_every_scan_reserve_and_rebase_checkpoint() {
        let text = format!(
            "Call Finish({},{},\"{}\" _\r\n,{}True)",
            "9".repeat(1024),
            "value".repeat(256),
            "x\"\"".repeat(512),
            " _\n".repeat(64)
        );
        let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
        let range = 7..7 + text.len();
        let mut successful_checks = 0;
        let parsed = statement(&source, range.clone(), 4, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap()
        .unwrap();
        let ModuleBodyStatement::Call(call) = parsed else {
            panic!("complete call expected");
        };
        assert_eq!(call.argument_values().len(), 4);
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            assert_eq!(
                statement(&source, range.clone(), 4, &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err(ModuleError::Cancelled("stop")),
                "checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
    }

    #[test]
    fn unsupported_call_forms_and_nonempty_tails_never_return_a_prefix() {
        for source in [
            "Call",
            "Call ",
            "Call()",
            "Call[Finish]",
            "Call\x0cFinish()",
            "Call\x0bFinish()",
            "Call\nFinish()",
            "Call_Finish()",
            "Call 1",
            "Call \"Finish\"",
            "Call [open",
            "Call []",
            "Call [a b]",
            "Call Finish$()",
            "Call Finish!",
            "Call Me",
            "Call True",
            "Call Nothing()",
            "Call Rem",
            "Call Sub()",
            "Call Name",
            "Call Name()",
            "cAlL nAmE",
            "cAlL nAmE()",
            "Call _\n Name()",
            "Call .Finish",
            "Call holder.Finish()",
            "Call holder!Finish",
            "Call Finish(value.member)",
            "Call Finish(value:=1)",
            "Call Finish(,)",
            "Call Finish(())",
            "Call Finish(\"\" extra)",
            "Call Finish( ' comment\n)",
            "Call Finish(\n)",
            "Call Finish(\r\n)",
            "Call Finish(",
            "Call Finish)",
            "Call Finish()()",
            "Call Finish().member",
            "Call Finish() = 1",
            "Call Finish = 1",
            "Call Finish + other",
            "Call Finish other",
            "Call Finish _",
            "Call Finish() trailing",
            "Call Finish:other=1",
            "Call Finish ' comment",
            "Finish",
            "Finish()",
        ] {
            assert!(read(source.as_bytes()).is_none(), "{source}");
        }
    }

    #[test]
    fn call_offsets_and_all_separator_and_suffix_bytes_remain_exact() {
        let text = b"Call [Finish]( _\r\n )";
        let source = [b"prefix\n".as_slice(), text, b"\ntail"].concat();
        let range = 7..7 + text.len();
        let ModuleBodyStatement::Call(call) =
            statement(&source, range.clone(), 0, &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap()
        else {
            panic!("explicit call expected")
        };
        assert_eq!(call.statement(), range);
        assert_eq!(&source[call.callee()], b"[Finish]");
        assert_eq!(&source[call.arguments().unwrap()], b"( _\r\n )");
        for byte in 0..=u8::MAX {
            let separator = [b"Call".as_slice(), &[byte], b"Finish()"].concat();
            assert_eq!(
                read(&separator).is_some(),
                matches!(byte, b' ' | b'\t'),
                "separator byte {byte}"
            );
            let suffix = [b"Call Finish()".as_slice(), &[byte]].concat();
            let result = read(&suffix);
            assert_eq!(
                result.is_some(),
                matches!(byte, b' ' | b'\t'),
                "suffix byte {byte}"
            );
            if let Some(ModuleBodyStatement::Call(call)) = result {
                assert!(call.statement().end <= suffix.len());
                assert!(call.callee().end <= suffix.len());
                assert!(call.arguments().unwrap().end <= suffix.len());
            }
        }
    }

    #[test]
    fn bare_statement_keyword_assignment_heads_are_rejected_case_insensitively() {
        for keyword in [
            "Load",
            "Unload",
            "Input",
            "Print",
            "Close",
            "Reset",
            "Beep",
            "DefBool",
            "DefByte",
            "DefInt",
            "DefLng",
            "DefLngLng",
            "DefLngPtr",
            "DefCur",
            "DefSng",
            "DefDbl",
            "DefDate",
            "DefStr",
            "DefObj",
            "DefVar",
            "DefDec",
        ] {
            for spelling in [
                keyword.to_owned(),
                keyword.to_ascii_lowercase(),
                keyword.to_ascii_uppercase(),
            ] {
                let source = format!("{spelling}=1");
                assert!(read(source.as_bytes()).is_none(), "{source}");
            }
            let identifier = format!("{keyword}Value=1");
            assert!(matches!(
                read(identifier.as_bytes()),
                Some(ModuleBodyStatement::Assignment(_))
            ));
        }
    }

    #[test]
    fn unsupported_local_forms_and_value_tails_never_return_a_prefix() {
        for source in [
            "Dim count As",
            "Dim count As Product",
            "Dim count As [Long]",
            "Dim count As Long()",
            "Dim count As New Object",
            "Dim count As String * 8",
            "Dim count As Long = 1",
            "Dim count$ As String",
            "Dim Me As Long",
            "Dim Dim As Long",
            "Static count As Long",
            "Private Dim count As Long",
            "Const Limit As Long",
            "Const Limit As Long = other",
            "Const Limit As Long = 1, Other = 2",
            "Const Limit As Long = --1",
            "count = -1.5e",
            "count = +1.0d",
            "count = 1.0e+",
            "count = 1E+",
            "count = 10.5d-",
            "count = 1&&",
            "count = 1more",
            "count = \"closed\" extra",
            "count = \"open",
            "count = \"continued _\n",
            "count = \"physical\nline\"",
            "count = (1",
            "count = 1 +",
            "count = target()()",
            "count = target.member",
            "count = Me",
            "count = Nothing",
            "count = Empty",
            "count = Null",
            "count = Not",
            "Me = count",
            "Rem = count",
            "Load=1",
            "Unload=1",
            "lOaD = 1",
            "uNlOaD = 1",
            "Load _\n = 1",
            "Unload _\r\n = 1",
            "count$ = 1",
            "count(0) = 1",
            "item.member = count",
            "Set item = other",
            "If flag Then",
            "Let = 1",
            "count = TrueValue extra",
            "count = True.False",
            "count = [open",
            "count = [a b]",
        ] {
            assert!(read(source.as_bytes()).is_none(), "{source}");
        }
    }

    #[test]
    fn expression_assignments_admit_former_scalar_and_application_boundary_controls() {
        for source in [
            "count=-1",
            "count=+1",
            "count=(1)",
            "count=1+2",
            "count=1-2",
            "count=Not True",
            "count=True And False",
            "count=Read((1))",
            "count=Read(target())",
            "count=Read(1+2)",
            "count=Read(True And False)",
            "count=Read(-1,+1)",
            "count=Read(1)+2",
            "count=Read(1)=2",
            "count=(Read(1))",
            "count=Read(1)=2=3",
            "count=a<b<c",
            "count=a<b Xor c>d Eqv e=f Imp g=h",
        ] {
            let Some(ModuleBodyStatement::ExpressionAssignment(assignment)) =
                read(source.as_bytes())
            else {
                panic!("newly supported expression {source}")
            };
            assert_eq!(assignment.statement(), 0..source.len());
            assert_eq!(&source[assignment.destination()], "count");
            assert!(
                assignment.expression().start >= 6 && assignment.expression().end == source.len()
            );
            assert!(assignment.inventory().work_nodes() > 0);
        }
    }

    #[test]
    fn expression_support_does_not_widen_const_initializers_or_parameter_defaults() {
        for source in [
            "Const Limit As Long=--1",
            "Const Limit As Long=1+2",
            "Const Limit As Long=(1)",
            "Const Limit As Long=Read(1)",
        ] {
            assert!(read(source.as_bytes()).is_none(), "{source}");
        }
        for source in [b"--1".as_slice(), b"1+2", b"(1)", b"Read(1)"] {
            let mut cursor = 0;
            let atom =
                parameter_default_value(source, &mut cursor, &mut || Ok::<(), ()>(())).unwrap();
            assert!(atom.is_none() || cursor < source.len());
        }
    }

    #[test]
    fn offsets_are_module_relative_and_all_arbitrary_suffixes_remain_bounded() {
        let source = b"prefix\nDim count As Long\ntail";
        let ModuleBodyStatement::Declaration(declaration) =
            statement(source, 7..24, 0, &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap()
        else {
            panic!("declaration expected")
        };
        assert_eq!(&source[declaration.name()], b"count");
        assert_eq!(&source[declaration.type_range()], b"Long");
        for byte in 0..=u8::MAX {
            let source = [b"count = 1".as_slice(), &[byte]].concat();
            if let Some(statement) = read(&source) {
                assert!(statement.statement().end <= source.len());
                let ModuleBodyStatement::Assignment(assignment) = statement else {
                    panic!("assignment expected")
                };
                assert!(assignment.destination().end <= source.len());
                assert!(assignment.value().span().end <= source.len());
            }
        }
    }

    #[test]
    fn bounded_storage_exhaustion_and_fallible_capacity_failures_preserve_entries() {
        for source in [
            b"count=1".as_slice(),
            b"Call Finish",
            b"Call Finish()",
            b"count=Read(1)",
        ] {
            let mut statements = Vec::new();
            assert_eq!(
                reserve::<()>(&mut statements, 0),
                Err(ModuleError::StatementLimit)
            );
            reserve::<()>(&mut statements, 1).unwrap();
            statements.push(read(source).unwrap());
            let snapshot = statements.clone();
            assert_eq!(
                reserve::<()>(&mut statements, 1),
                Err(ModuleError::StatementLimit)
            );
            assert_eq!(
                reserve_exact::<()>(&mut statements, usize::MAX),
                Err(ModuleError::AllocationFailed)
            );
            assert_eq!(statements, snapshot);
        }
    }

    #[test]
    fn scans_cancel_inside_long_names_strings_numbers_and_continuations() {
        for source in [
            format!("count={}", "9".repeat(8192)),
            format!("count=\"{}\"", "x\"\"".repeat(4096)),
            format!("Dim {} As Long", "x".repeat(8192)),
            format!("count {}=1", " _\n".repeat(8192)),
        ] {
            for limit in [1, 8] {
                let mut calls = 0;
                assert_eq!(
                    statement(source.as_bytes(), 0..source.len(), 0, &mut || {
                        calls += 1;
                        if calls == limit { Err("stop") } else { Ok(()) }
                    }),
                    Err(ModuleError::Cancelled("stop"))
                );
                assert_eq!(calls, limit);
            }
        }
    }

    #[test]
    fn call_scans_cancel_inside_callees_and_each_logical_trivia_position() {
        for source in [
            format!("Call {}()", "x".repeat(8192)),
            format!("Call {}Finish", " _\r\n".repeat(8192)),
            format!("Call Finish{}()", " _\n".repeat(8192)),
            format!("Call Finish({})", " _\r\n".repeat(8192)),
            format!("Call Finish(){}", " _\n".repeat(8192)),
        ] {
            assert!(matches!(
                read(source.as_bytes()),
                Some(ModuleBodyStatement::Call(_))
            ));
            for limit in [1, 8] {
                let mut calls = 0;
                assert_eq!(
                    statement(source.as_bytes(), 0..source.len(), 0, &mut || {
                        calls += 1;
                        if calls == limit { Err("stop") } else { Ok(()) }
                    }),
                    Err(ModuleError::Cancelled("stop"))
                );
                assert_eq!(calls, limit);
            }
        }
    }

    #[test]
    fn application_assignments_retain_all_original_ranges_and_scalar_categories() {
        for (text, destination, callee, expected) in [
            ("count=Read()", "count", "Read", vec![]),
            ("count=target()", "count", "target", vec![]),
            ("count=[Line]()", "count", "[Line]", vec![]),
            (
                "Let [count] = [Read](007,\"a,:='Rem End Sub \"\"q\"\"\",True,[Me],TrueRem,[True])",
                "[count]",
                "[Read]",
                vec![
                    ("007", ModuleScalarValueKind::UnsignedDecimal),
                    (
                        "\"a,:='Rem End Sub \"\"q\"\"\"",
                        ModuleScalarValueKind::String,
                    ),
                    ("True", ModuleScalarValueKind::Boolean),
                    ("[Me]", ModuleScalarValueKind::Name),
                    ("TrueRem", ModuleScalarValueKind::Name),
                    ("[True]", ModuleScalarValueKind::Name),
                ],
            ),
            (
                "lEt _\r\n [count] _\n = _\r\n [Read] _\n ( _\r\n False _\n , _\r\n value _\n )",
                "[count]",
                "[Read]",
                vec![
                    ("False", ModuleScalarValueKind::Boolean),
                    ("value", ModuleScalarValueKind::Name),
                ],
            ),
        ] {
            let prefix = b"prefix\r\n";
            let source = [prefix.as_slice(), text.as_bytes(), b"\ntail"].concat();
            let range = prefix.len()..prefix.len() + text.len();
            let ModuleBodyStatement::ApplicationAssignment(assignment) =
                statement(&source, range.clone(), expected.len(), &mut || {
                    Ok::<(), ()>(())
                })
                .unwrap()
                .unwrap()
            else {
                panic!("complete application assignment expected")
            };
            assert_eq!(assignment.statement(), range.clone());
            assert_eq!(&source[assignment.destination()], destination.as_bytes());
            assert_eq!(&source[assignment.callee()], callee.as_bytes());
            assert_eq!(assignment.application().start, assignment.callee().start);
            assert_eq!(assignment.application().end, assignment.arguments().end);
            assert_eq!(assignment.application().end, range.end);
            assert!(assignment.callee().end <= assignment.arguments().start);
            assert_eq!(source[assignment.arguments().start], b'(');
            assert_eq!(source[assignment.arguments().end - 1], b')');
            assert_eq!(assignment.argument_values().len(), expected.len());
            let mut previous_end = assignment.arguments().start + 1;
            for (value, (spelling, kind)) in assignment.argument_values().iter().zip(expected) {
                assert_eq!(&source[value.span()], spelling.as_bytes());
                assert_eq!(value.kind(), kind);
                assert!(previous_end <= value.span().start);
                assert!(value.span().end < assignment.arguments().end);
                previous_end = value.span().end;
            }
        }
    }

    #[test]
    fn application_assignments_retain_many_arguments_without_singleton_shortcuts() {
        let atoms = (0..256)
            .map(|number| number.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let text = format!("count=Read({atoms})");
        let ModuleBodyStatement::ApplicationAssignment(assignment) = read(text.as_bytes()).unwrap()
        else {
            panic!("many-argument application expected")
        };
        assert_eq!(assignment.argument_values().len(), 256);
        for (index, value) in assignment.argument_values().iter().enumerate() {
            assert_eq!(&text[value.span()], index.to_string());
            assert_eq!(value.kind(), ModuleScalarValueKind::UnsignedDecimal);
        }
    }

    #[test]
    fn application_assignment_malformed_atoms_and_richer_tails_never_return_a_prefix() {
        for atom in [
            "",
            " ",
            ",",
            "value:=1",
            "[value]:=1",
            "ByVal value",
            "ByRef value",
            "(1",
            "()",
            "target().member",
            "target.member",
            "target!member",
            "1+",
            "True And",
            "-1.0e",
            "+1.5d",
            "1.0e+",
            ".5d-",
            "1E+",
            "10.5e-",
            "7.5d+",
            "#1/1/2000#",
            "1&&",
            "value$",
            "value!",
            "Nothing",
            "Null",
            "Empty",
            "Me",
            "\"open",
            "\"continued _\n",
            "\"physical\nline\"",
            "\"closed\" extra",
            "[open",
            "[]",
            "[value name]",
            "1more",
            "True False",
            "1;2",
            "1\n",
        ] {
            let text = format!("count=Read(0,{atom},2)");
            assert!(read(text.as_bytes()).is_none(), "{atom:?}");
        }
        for text in [
            "count=Line()",
            "count=line(1)",
            "Let count=lInE(value)",
            "count=Read(1,)",
            "count=Read(,1)",
            "count=Read(1,,2)",
            "count=Read(1, _\n )",
            "count=Read(1",
            "count=Read(1) extra",
            "count=Read(1)()",
            "count=Read(1).member",
            "count=Read(1)!member",
            "count=Read(1)+2.5e",
            "count=Read(1)=2=3.0d",
            "count=Read 1",
            "count=(Read(1) extra)",
            "count=holder.Read(1)",
            "count=.Read(1)",
            "count=Read$()",
            "count=True()",
            "count=Me()",
            "count=Nothing()",
            "count(1)=Read()",
            "item.member=Read()",
            "Set item=Read()",
            "Load=Read()",
            "count=Read(\n)",
            "count=Read(\r\n)",
            "count=Read( ' comment\n)",
            "count=Read() Rem tail",
            "count=Read() ' tail",
            "Const Limit As Long=Read()",
            "Dim count As Long=Read()",
        ] {
            assert!(read(text.as_bytes()).is_none(), "{text:?}");
        }
    }

    #[test]
    fn application_assignment_argument_limits_preserve_typed_failures_and_empty_syntax() {
        for (text, maximum, count) in [
            ("count=Read()", 0, 0),
            ("Let count=Read( _\r\n )", 0, 0),
            ("count=Read(1,value,True)", 3, 3),
        ] {
            let ModuleBodyStatement::ApplicationAssignment(assignment) =
                statement(text.as_bytes(), 0..text.len(), maximum, &mut || {
                    Ok::<(), ()>(())
                })
                .unwrap()
                .unwrap()
            else {
                panic!("complete application assignment expected")
            };
            assert_eq!(assignment.argument_values().len(), count);
        }
        let text = b"count=Read(1,value,True)";
        for maximum in [0, 1, 2] {
            assert_eq!(
                statement(text, 0..text.len(), maximum, &mut || Ok::<(), ()>(())),
                Err(ModuleError::ArgumentLimit)
            );
        }
        let mut values = Vec::new();
        assert_eq!(
            reserve_arguments_exact(&mut values, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert!(values.is_empty());
    }

    #[test]
    fn application_assignment_all_byte_boundaries_require_complete_original_tokens() {
        for byte in 0..=u8::MAX {
            for (prefix, suffix, expected) in [
                (
                    b"count=Read(1".as_slice(),
                    b")".as_slice(),
                    byte.is_ascii_digit()
                        || matches!(
                            byte,
                            b' ' | b'\t' | b'%' | b'&' | b'^' | b'.' | b'!' | b'#' | b'@'
                        ),
                ),
                (
                    b"count=Read(1,",
                    b"2)",
                    byte.is_ascii_alphanumeric()
                        || matches!(byte, b'_' | b' ' | b'\t' | b'+' | b'-' | b'&' | b'.'),
                ),
                (b"count=Read(1)", b"", matches!(byte, b' ' | b'\t')),
                (
                    b"count=Read",
                    b"(1)",
                    byte.is_ascii_alphanumeric()
                        || matches!(
                            byte,
                            b'_' | b' '
                                | b'\t'
                                | b'+'
                                | b'-'
                                | b'*'
                                | b'/'
                                | b'\\'
                                | b'='
                                | b'<'
                                | b'>'
                        ),
                ),
                (
                    b"count",
                    b"=Read()",
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b' ' | b'\t'),
                ),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let result = read(&source);
                assert_eq!(
                    result.is_some(),
                    expected,
                    "{prefix:?} byte {byte} {suffix:?}"
                );
                if let Some(parsed) = result {
                    if let ModuleBodyStatement::ExpressionAssignment(assignment) = &parsed {
                        assert_eq!(assignment.statement(), 0..source.len());
                        if prefix == b"count=Read" {
                            assert!(matches!(
                                byte,
                                b'+' | b'-' | b'*' | b'/' | b'\\' | b'=' | b'<' | b'>'
                            ));
                            assert_eq!(&source[assignment.destination()], b"count");
                            assert_eq!(assignment.expression(), 6..source.len());
                            assert!(assignment.inventory().applications().is_empty());
                            assert_eq!(assignment.inventory().argument_count(), 0);
                            assert_eq!(assignment.inventory().work_nodes(), 4);
                            let values = assignment.inventory().values();
                            assert_eq!(values.len(), 2);
                            assert_eq!(&source[values[0].span()], b"Read");
                            assert_eq!(values[0].kind(), ModuleScalarValueKind::Name);
                            assert_eq!(&source[values[1].span()], b"1");
                            assert_eq!(values[1].kind(), ModuleScalarValueKind::UnsignedDecimal);
                            continue;
                        }
                        assert_eq!(prefix, b"count=Read(1,");
                        assert!(matches!(byte, b'+' | b'-'));
                        let application = &assignment.inventory().applications()[0];
                        assert_eq!(application.argument_expressions().len(), 2);
                        assert_eq!(application.application().end, source.len());
                        assert_eq!(assignment.inventory().work_nodes(), 5);
                        continue;
                    }
                    let ModuleBodyStatement::ApplicationAssignment(assignment) = parsed else {
                        panic!("accepted token boundary must retain the whole application")
                    };
                    assert_eq!(assignment.statement(), 0..source.len());
                    for range in [
                        assignment.destination(),
                        assignment.callee(),
                        assignment.application(),
                        assignment.arguments(),
                    ] {
                        assert!(range.start < range.end && range.end <= source.len());
                    }
                    assert_eq!(source[assignment.arguments().start], b'(');
                    assert_eq!(source[assignment.arguments().end - 1], b')');
                    for value in assignment.argument_values() {
                        assert!(assignment.arguments().start < value.span().start);
                        assert!(value.span().start < value.span().end);
                        assert!(value.span().end < assignment.arguments().end);
                    }
                }
            }
        }
    }

    #[test]
    fn application_assignment_cancellation_survives_every_scan_reserve_and_rebase_checkpoint() {
        let text = format!(
            "Let [count] = [Read] _\r\n ({},{},\"{}\" _\n,{}True)",
            "9".repeat(1024),
            "value".repeat(256),
            "x\"\"".repeat(512),
            " _\r\n".repeat(32)
        );
        let source = [b"prefix\n".as_slice(), text.as_bytes(), b"\ntail"].concat();
        let range = 7..7 + text.len();
        let mut checkpoints = 0;
        let parsed = statement(&source, range.clone(), 4, &mut || {
            checkpoints += 1;
            Ok::<(), ()>(())
        })
        .unwrap()
        .unwrap();
        let ModuleBodyStatement::ApplicationAssignment(assignment) = parsed else {
            panic!("complete application assignment expected")
        };
        assert_eq!(assignment.argument_values().len(), 4);
        for cancelled_at in 1..=checkpoints {
            let mut calls = 0;
            assert_eq!(
                statement(&source, range.clone(), 4, &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err(ModuleError::Cancelled("stop")),
                "checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
    }
}
