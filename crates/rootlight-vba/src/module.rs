//! Inventories a closed procedural VBA module without a parser runtime.
//! Acceptance covers the entire input; an unsupported statement cannot expose
//! a partial structural inventory for an adapter to mistake for complete facts.

use std::{fmt, ops::Range};

use super::parameter::parameter_trivia_len;
use super::{
    ModuleBodyStatement, ModuleScalarValue, ProcedureKind, logical_whitespace_len, procedure_header,
};

/// Caller-owned ceilings for one complete module scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleLimits {
    max_source_bytes: usize,
    max_statements: usize,
    max_procedures: usize,
    max_parameters: usize,
    max_arguments: usize,
    max_expression_nodes: usize,
}

impl ModuleLimits {
    /// Creates source-byte, logical-statement and retained-procedure ceilings.
    /// Zero ceilings are valid and admit only inputs requiring no such resource.
    /// Aggregate parameter and argument ceilings default to the procedure ceiling;
    /// use [`Self::with_max_parameters`] and [`Self::with_max_arguments`] to admit
    /// separately bounded comma lists.
    /// Module constants, variables and local Dim members share the retained-record
    /// ceiling with other body records, independently of consumed logical segments.
    /// Structured-expression work defaults to the source-byte ceiling; old scalar,
    /// scalar-application and atomic Call statements do not consume that ceiling.
    #[must_use]
    pub const fn new(
        max_source_bytes: usize,
        max_statements: usize,
        max_procedures: usize,
    ) -> Self {
        Self {
            max_source_bytes,
            max_statements,
            max_procedures,
            max_parameters: max_procedures,
            max_arguments: max_procedures,
            max_expression_nodes: max_source_bytes,
        }
    }

    /// Sets the aggregate retained-parameter ceiling across the entire module.
    /// This is independent of the logical-statement ceiling: a single header can
    /// contain many parameters. A zero ceiling still admits empty parameter lists.
    #[must_use]
    pub const fn with_max_parameters(mut self, max_parameters: usize) -> Self {
        self.max_parameters = max_parameters;
        self
    }

    /// Sets the aggregate retained-argument ceiling across Calls and application assignments.
    /// A logical statement can contain many arguments, so this ceiling is separate
    /// from statement count. Zero still admits bare/empty Calls and empty applications.
    #[must_use]
    pub const fn with_max_arguments(mut self, max_arguments: usize) -> Self {
        self.max_arguments = max_arguments;
        self
    }

    /// Sets aggregate structured-expression work across the entire module.
    /// Atoms, callees, applications, groups and unary/binary operators each cost
    /// one node. Both structured assignments and expression Calls share this
    /// ceiling. Storage is bounded and fallible, not an exact heap-byte budget.
    /// Existing scalar, scalar-application and atomic Call records are unchanged.
    #[must_use]
    pub const fn with_max_expression_nodes(mut self, max_expression_nodes: usize) -> Self {
        self.max_expression_nodes = max_expression_nodes;
        self
    }
}

/// A complete structural inventory for the supported procedural module slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleInventory {
    source_len: usize,
    module_name: Option<Range<usize>>,
    procedures: Vec<ModuleProcedure>,
    constants: Vec<ModuleConstant>,
    variables: Vec<ModuleVariable>,
    statement_count: usize,
}

impl ModuleInventory {
    /// Returns the length of the unchanged source to which all ranges belong.
    #[must_use]
    pub const fn source_len(&self) -> usize {
        self.source_len
    }

    /// Returns the ASCII module name inside the `VB_Name` string's quotes.
    #[must_use]
    pub fn module_name(&self) -> Option<Range<usize>> {
        self.module_name.clone()
    }

    /// Returns complete procedures in source order, never a partial prefix.
    #[must_use]
    pub fn procedures(&self) -> &[ModuleProcedure] {
        &self.procedures
    }

    /// Returns complete module-level constants in source order, not local declarations.
    #[must_use]
    pub fn constants(&self) -> &[ModuleConstant] {
        &self.constants
    }

    /// Returns complete module-level variables in source order, not local declarations.
    /// Comma-list members stay flat and share their complete statement range.
    #[must_use]
    pub fn variables(&self) -> &[ModuleVariable] {
        &self.variables
    }

    /// Returns consumed logical segments, including blank and comment segments.
    /// A trailing EOF does not add a synthetic segment.
    #[must_use]
    pub const fn statement_count(&self) -> usize {
        self.statement_count
    }
}

/// The authored visibility keyword on a module-level variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleVariableVisibility {
    /// The statement explicitly carries `Public`.
    Public,
    /// The statement explicitly carries `Private`.
    Private,
}

/// One uninitialized module-level variable retaining scalar or numeric-array syntax.
/// An absent As clause or a Dim prefix does not fabricate Variant type or Private
/// visibility. Every range addresses the original module rather than copied text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleVariable {
    statement: Range<usize>,
    declarator: Range<usize>,
    name: Range<usize>,
    array_bounds: Option<Range<usize>>,
    array_work_nodes: usize,
    type_range: Option<Range<usize>>,
    visibility: Option<ModuleVariableVisibility>,
    visibility_range: Option<Range<usize>>,
}

impl ModuleVariable {
    /// Returns the complete trimmed statement, including its declaration prefix.
    /// Every declarator in one comma list shares this range.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }

    /// Returns the name through authored array bounds or type, excluding its prefix.
    /// Without either bounds or type, this range is exactly the name token.
    #[must_use]
    pub fn declarator(&self) -> Range<usize> {
        self.declarator.clone()
    }

    /// Returns the authored variable name token, retaining brackets when present.
    #[must_use]
    pub fn name(&self) -> Range<usize> {
        self.name.clone()
    }

    /// Returns the complete numeric array designator, including both parentheses.
    /// None means scalar syntax; an authored empty group is Some, never omitted.
    /// Endpoint magnitudes, lower-bound defaults and allocation size are not evaluated.
    #[must_use]
    pub fn array_bounds(&self) -> Option<Range<usize>> {
        self.array_bounds.clone()
    }

    /// Returns logical work for the authored numeric array designator.
    /// Scalar syntax costs zero. A group costs one node, as does each dimension,
    /// numeric endpoint and authored unary sign; no value or array size is evaluated.
    #[must_use]
    pub const fn array_work_nodes(&self) -> usize {
        self.array_work_nodes
    }

    /// Returns the written intrinsic type, or None without a written As clause.
    /// None is source absence, never an empty range or a synthetic Variant token.
    #[must_use]
    pub fn type_range(&self) -> Option<Range<usize>> {
        self.type_range.clone()
    }

    /// Returns written Public/Private visibility, or None for an authored Dim.
    #[must_use]
    pub const fn visibility(&self) -> Option<ModuleVariableVisibility> {
        self.visibility
    }

    /// Returns the authored visibility token, excluding its following trivia.
    /// A Dim statement has no visibility token and returns None.
    #[must_use]
    pub fn visibility_range(&self) -> Option<Range<usize>> {
        self.visibility_range.clone()
    }
}

/// The authored visibility keyword on a module-level constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleConstantVisibility {
    /// The statement explicitly carries `Public`.
    Public,
    /// The statement explicitly carries `Private`.
    Private,
}

/// One module-level scalar constant with a complete literal initializer.
/// All ranges address the original module; no inferred type, visibility or
/// evaluated constant value is substituted for absent source syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleConstant {
    statement: Range<usize>,
    declarator: Range<usize>,
    name: Range<usize>,
    type_range: Option<Range<usize>>,
    value: ModuleScalarValue,
    visibility: Option<ModuleConstantVisibility>,
    visibility_range: Option<Range<usize>>,
}

impl ModuleConstant {
    /// Returns the complete trimmed statement, including any visibility keyword.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }

    /// Returns the exact name through initializer, excluding visibility and Const.
    #[must_use]
    pub fn declarator(&self) -> Range<usize> {
        self.declarator.clone()
    }

    /// Returns the exact authored constant name, including any brackets.
    #[must_use]
    pub fn name(&self) -> Range<usize> {
        self.name.clone()
    }

    /// Returns the written intrinsic type keyword, or None for absent As syntax.
    #[must_use]
    pub fn type_range(&self) -> Option<Range<usize>> {
        self.type_range.clone()
    }

    /// Returns the authored literal token without evaluating or coercing it.
    #[must_use]
    pub fn value(&self) -> &ModuleScalarValue {
        &self.value
    }

    /// Returns only a written Public or Private keyword, never inferred visibility.
    #[must_use]
    pub const fn visibility(&self) -> Option<ModuleConstantVisibility> {
        self.visibility
    }

    /// Returns the exact visibility keyword token, or None when none was written.
    #[must_use]
    pub fn visibility_range(&self) -> Option<Range<usize>> {
        self.visibility_range.clone()
    }
}

/// A written scalar parameter passing modifier, without inferred defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleParameterPassingMode {
    /// The parameter explicitly carries `ByVal`.
    ByVal,
    /// The parameter explicitly carries `ByRef`.
    ByRef,
}

/// One scalar parameter retaining its authored intrinsic type or type absence.
/// Optional parameters retain an absent or atomic authored default without
/// evaluation. Array bounds and type suffixes are excluded. All ranges address
/// the entire module, never a reconstructed parameter list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleParameter {
    declaration: Range<usize>,
    name: Range<usize>,
    type_range: Option<Range<usize>>,
    passing_mode: Option<ModuleParameterPassingMode>,
    is_optional: bool,
    default_value: Option<ModuleScalarValue>,
}

impl ModuleParameter {
    /// Returns the first written modifier or name through the last authored token.
    /// Surrounding parentheses and trailing logical trivia are excluded.
    #[must_use]
    pub fn declaration(&self) -> Range<usize> {
        self.declaration.clone()
    }

    /// Returns the exact authored parameter name, including any brackets.
    #[must_use]
    pub fn name(&self) -> Range<usize> {
        self.name.clone()
    }

    /// Returns the authored intrinsic type keyword without preceding `As` trivia.
    /// None means no type clause was written, not an inferred Variant identity
    /// or an empty source range. A written but invalid As clause is unsupported.
    #[must_use]
    pub fn type_range(&self) -> Option<Range<usize>> {
        self.type_range.clone()
    }

    /// Returns the written passing modifier, or None when no modifier is written.
    /// Omission is retained as source evidence rather than inferred as `ByRef`.
    #[must_use]
    pub const fn passing_mode(&self) -> Option<ModuleParameterPassingMode> {
        self.passing_mode
    }

    /// Returns whether the parameter explicitly carries the Optional keyword.
    /// This is source syntax, not an inferred runtime value or passing mode.
    #[must_use]
    pub const fn is_optional(&self) -> bool {
        self.is_optional
    }

    /// Returns the authored scalar literal or name, or None when none is written.
    /// Absence is never replaced by an inferred value; names require independent
    /// binding. The atom proves neither type compatibility nor constant validity.
    #[must_use]
    pub fn default_value(&self) -> Option<&ModuleScalarValue> {
        self.default_value.as_ref()
    }
}

/// One complete closed-slice Sub or Function retaining original source offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleProcedure {
    kind: ProcedureKind,
    declaration: Range<usize>,
    header: Range<usize>,
    name: Range<usize>,
    parameters: Vec<ModuleParameter>,
    body: Range<usize>,
    terminator: Range<usize>,
    statements: Vec<ModuleBodyStatement>,
}

impl ModuleProcedure {
    /// Returns the written Sub or Function category.
    #[must_use]
    pub const fn kind(&self) -> ProcedureKind {
        self.kind
    }

    /// Returns the declaration from its first modifier through its End keyword.
    /// Trailing terminator comments and line endings are excluded.
    #[must_use]
    pub fn declaration(&self) -> Range<usize> {
        self.declaration.clone()
    }

    /// Returns the exact trimmed logical header, including legal continuations.
    #[must_use]
    pub fn header(&self) -> Range<usize> {
        self.header.clone()
    }

    /// Returns the authored name token, including brackets or a type suffix.
    #[must_use]
    pub fn name(&self) -> Range<usize> {
        self.name.clone()
    }

    /// Returns complete scalar parameters in source order, including empty lists.
    /// Aggregate retention is bounded separately from logical statement count.
    /// Unsupported lists never expose a prefix.
    #[must_use]
    pub fn parameters(&self) -> &[ModuleParameter] {
        &self.parameters
    }

    /// Returns the body between the consumed header boundary and End statement.
    #[must_use]
    pub fn body(&self) -> Range<usize> {
        self.body.clone()
    }

    /// Returns the exact trimmed matching End Sub or End Function statement.
    #[must_use]
    pub fn terminator(&self) -> Range<usize> {
        self.terminator.clone()
    }

    /// Returns complete nontrivia body statements in original source order.
    #[must_use]
    pub fn statements(&self) -> &[ModuleBodyStatement] {
        &self.statements
    }
}

/// A whole-module rejection, resource failure or caller cancellation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleError<E> {
    /// Unsupported or malformed source; no partial inventory is returned.
    Unsupported {
        /// Offending source range, or the unmatched declaration through EOF.
        range: Range<usize>,
    },
    /// The source exceeds the caller's byte ceiling.
    SourceLimit,
    /// Consumed logical segments or aggregate retained declaration/body records
    /// exceed the caller's statement ceiling.
    StatementLimit,
    /// Declared procedures exceed the caller's retained-procedure ceiling.
    ProcedureLimit,
    /// Retained parameters across all procedures exceed the parameter ceiling.
    ParameterLimit,
    /// Retained Call/application arguments across the module exceed the argument ceiling.
    ArgumentLimit,
    /// Structured-expression work across the module exceeds its node ceiling.
    ExpressionLimit,
    /// Fallible reservation of bounded structural storage failed.
    AllocationFailed,
    /// The caller's cancellation or work callback failed.
    Cancelled(E),
}

impl<E> fmt::Display for ModuleError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported { .. } => "unsupported module syntax",
            Self::SourceLimit => "module source limit exceeded",
            Self::StatementLimit => "module statement limit exceeded",
            Self::ProcedureLimit => "module procedure limit exceeded",
            Self::ParameterLimit => "module parameter limit exceeded",
            Self::ArgumentLimit => "module argument limit exceeded",
            Self::ExpressionLimit => "module expression limit exceeded",
            Self::AllocationFailed => "module inventory allocation failed",
            Self::Cancelled(_) => "module scan cancelled",
        })
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ModuleError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cancelled(error) => Some(error),
            _ => None,
        }
    }
}

/// Parses the complete closed slice of a procedural VBA source module.
///
/// Supports one leading alpha-first ASCII `Attribute VB_Name` of at most 31
/// bytes, unique legal Option categories,
/// blank/comment segments and closed-slice Sub/Function procedures. Sub parameter
/// lists may be absent; Function lists require parentheses. Explicit lists are
/// empty or contain a complete comma list of scalar parameters with absent
/// type clauses or explicit intrinsic `As` types and written `ByVal`/`ByRef`.
/// Type absence is preserved without a synthetic Variant source or identity.
/// Optional parameters carry the keyword before any passing modifier and may
/// omit an authored default or retain one unsigned decimal, physically closed
/// string, Boolean literal or simple name requiring independent constant binding.
/// Parameter-token boundaries also retain form-feed trivia and bare Rem comments;
/// a comment-only parenthesized list is empty, never a synthetic Rem parameter.
/// Rem comments with space/tab tails, bare line endings and missing comma-list
/// elements are unsupported rather than skipped into a partial list.
/// Defaults on required parameters, other default expressions, arrays, `ParamArray`
/// and type-suffixed parameter names are unsupported. Functions retain
/// an optional intrinsic scalar As return clause or unbracketed type suffix.
/// Public/Private and optional Static modifiers are accepted in that order.
/// Module-level Const statements retain one typed or untyped scalar literal
/// initializer and optional Public/Private spelling. They may precede, separate
/// or follow procedures, but Attribute and Option remain leading-header forms.
/// A module-level Dim, Public or Private statement retains a complete comma list
/// of uninitialized variables, each with an absent or written intrinsic As type.
/// Members share statement and visibility ranges, not types or array bounds.
/// Array bounds may be empty or a complete comma list of signed/unsigned decimal
/// endpoints, each upper-only or lower To upper. Magnitudes and runtime array sizes are not
/// evaluated, and no per-dimension records are retained. Named/expression bounds,
/// initializers, incomplete declaration lists and richer prefixes remain unsupported.
/// Body Dim lists retain the same per-member scalar/numeric-array syntax in
/// their containing procedure, without module visibility or inferred types.
/// A single typed scalar Dim keeps its distinct legacy declaration record.
/// Local Const still admits one typed scalar per statement. Assignments have
/// an optional Let prefix. Const values are unsigned decimal,
/// physically closed doubled-quote strings or True/False; assignments additionally
/// admit names, grouped expressions, unary signs/Not, ordinary arithmetic,
/// concatenation, comparison and logical operators, and nested simple-name applications.
/// Applications retain complete positional expression ranges without deciding whether
/// their targets are procedures or arrays. Old scalar and standalone scalar-application
/// records remain distinct. Explicit Call statements admit a simple callee and optional
/// parentheses containing a complete positional list of admitted expressions, with
/// space/tab-based logical trivia. Named, omitted, ByVal and nonparenthesized
/// arguments are unsupported. Adjacent literal/ordinary-name type suffixes stay
/// outside this slice instead of being reinterpreted as power or concatenation.
/// Richer literals, member applications and indexed
/// destinations remain unsupported. Complete operator chains inventory syntax
/// without deciding type compatibility or runtime results. Every body statement
/// must be consumed completely.
/// Bare `Name` explicit Call callees stay outside this subset; bracketed `[Name]` is admitted.
/// Without explicit `Let`, bare statement-keyword assignment destinations are excluded.
/// All other statements, inline Rem tails, directives, class
/// members, continued-comment forms and incomplete boundaries reject the entire input.
/// Colon boundaries are allowed only after a procedure opener or within its body.
/// Unbracketed Rem and Rem! names remain on the native route: the comment
/// keyword can consume the name before a separate bang-suffix token is formed.
///
/// Ranges always address the original bytes, preserving LF/CRLF and explicit
/// continuations. The scan is nonrecursive, bounded and cancellable before
/// admission, per statement and periodically within byte scans. Statement
/// ceilings include trivia segments; procedure and body storage is reserved fallibly.
/// Retained body, module-constant and module-variable records together cannot exceed
/// the statement ceiling; each local Dim member costs a separate record, while
/// its comma list remains one logical segment. Storage is fallible and bounded
/// before retaining siblings; this bounds records, not allocator bytes.
/// Retained parameters across all procedures have a separate aggregate ceiling;
/// their storage is reserved fallibly, never inferred from logical statement count.
/// Each parameter retains at most one inline default atom under that same ceiling.
/// Optionality and defaults are source evidence, not inferred values, coercion,
/// or proof of required-after-Optional semantic validity.
/// Retained Call and application-assignment arguments share an aggregate ceiling across every procedure,
/// with fallible storage; positional atoms and expressions do not establish
/// parameter or type identity. Nested expression applications share that ceiling.
/// Structured expressions have a separate aggregate node ceiling. Parsing and temporary
/// tree storage are iterative, fallible and cancellable; expressions are never evaluated.
/// This proves structure only, not type identity, visibility or name uniqueness.
///
/// # Errors
/// Returns [`ModuleError::Unsupported`] without a partial inventory for any
/// unsupported source, a typed limit/allocation error for resource exhaustion,
/// or [`ModuleError::Cancelled`] preserving the caller's callback error.
pub fn module_inventory<E>(
    source: &[u8],
    limits: ModuleLimits,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<ModuleInventory, ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if source.len() > limits.max_source_bytes {
        return Err(ModuleError::SourceLimit);
    }
    for (offset, bytes) in source.chunks(512).enumerate() {
        check().map_err(ModuleError::Cancelled)?;
        for (index, byte) in bytes.iter().enumerate() {
            let position = offset * 512 + index;
            if *byte == 0 || *byte == b'\r' && source.get(position + 1) != Some(&b'\n') {
                return Err(ModuleError::Unsupported {
                    range: position..position + 1,
                });
            }
        }
    }
    let mut inventory = ModuleInventory {
        source_len: source.len(),
        module_name: None,
        procedures: Vec::new(),
        constants: Vec::new(),
        variables: Vec::new(),
        statement_count: 0,
    };
    let mut open: Option<ModuleProcedure> = None;
    let mut cursor = 0;
    let mut options = 0_u8;
    let mut attribute_header = true;
    let mut declaration_started = false;
    let mut body_count = 0_usize;
    let mut parameter_count = 0_usize;
    let mut argument_count = 0_usize;
    let mut expression_count = 0_usize;
    while cursor < source.len() {
        check().map_err(ModuleError::Cancelled)?;
        if inventory.statement_count >= limits.max_statements {
            return Err(ModuleError::StatementLimit);
        }
        let Some((range, next)) = super::with_blocks::statement(source, cursor, false, check)
            .map_err(ModuleError::Cancelled)?
        else {
            return Err(ModuleError::Unsupported {
                range: cursor..source.len(),
            });
        };
        inventory.statement_count += 1;
        cursor = next;
        if !supported_comment_tail(source.get(range.end..next).unwrap_or_default(), check)
            .map_err(ModuleError::Cancelled)?
        {
            return Err(ModuleError::Unsupported {
                range: range.end..next,
            });
        }
        let text = source.get(range.clone()).unwrap_or_default();
        let colon_boundary = source
            .get(range.end..next)
            .is_some_and(|tail| tail.trim_ascii() == b":");
        if text.is_empty() {
            if open.is_none() && colon_boundary {
                return Err(ModuleError::Unsupported {
                    range: range.end..next,
                });
            }
            continue;
        }
        if let Some(procedure) = &mut open {
            if is_terminator(text, procedure.kind, check).map_err(ModuleError::Cancelled)? {
                if colon_boundary {
                    return Err(ModuleError::Unsupported { range });
                }
                procedure.body.end = range.start;
                procedure.declaration.end = range.end;
                procedure.terminator = range;
                if let Some(procedure) = open.take() {
                    inventory.procedures.push(procedure);
                }
            } else {
                let remaining_arguments = limits
                    .max_arguments
                    .checked_sub(argument_count)
                    .ok_or(ModuleError::ArgumentLimit)?;
                let remaining_expression_nodes = limits
                    .max_expression_nodes
                    .checked_sub(expression_count)
                    .ok_or(ModuleError::ExpressionLimit)?;
                let remaining_records = limits
                    .max_statements
                    .checked_sub(body_count)
                    .and_then(|remaining| remaining.checked_sub(inventory.constants.len()))
                    .and_then(|remaining| remaining.checked_sub(inventory.variables.len()))
                    .ok_or(ModuleError::StatementLimit)?;
                let Some(statement) = super::module_body::statement_with_limits(
                    source,
                    range.clone(),
                    super::module_body::BodyLimits {
                        arguments: remaining_arguments,
                        expression_nodes: remaining_expression_nodes,
                        records: remaining_records,
                    },
                    check,
                )?
                else {
                    return Err(ModuleError::Unsupported { range });
                };
                let retained_arguments = match &statement {
                    ModuleBodyStatement::Call(call) => call.argument_values().len(),
                    ModuleBodyStatement::ApplicationAssignment(assignment) => {
                        assignment.argument_values().len()
                    }
                    ModuleBodyStatement::ExpressionAssignment(assignment) => {
                        assignment.inventory().argument_count()
                    }
                    ModuleBodyStatement::ExpressionCall(call) => call.inventory().argument_count(),
                    ModuleBodyStatement::Declaration(_)
                    | ModuleBodyStatement::Variables(_)
                    | ModuleBodyStatement::Assignment(_) => 0,
                };
                argument_count = argument_count
                    .checked_add(retained_arguments)
                    .ok_or(ModuleError::ArgumentLimit)?;
                let retained_expression_nodes = match &statement {
                    ModuleBodyStatement::ExpressionAssignment(assignment) => {
                        assignment.inventory().work_nodes()
                    }
                    ModuleBodyStatement::ExpressionCall(call) => call.inventory().work_nodes(),
                    ModuleBodyStatement::Declaration(_)
                    | ModuleBodyStatement::Variables(_)
                    | ModuleBodyStatement::Assignment(_)
                    | ModuleBodyStatement::ApplicationAssignment(_)
                    | ModuleBodyStatement::Call(_) => 0,
                };
                if retained_expression_nodes != 0 {
                    expression_count = expression_count
                        .checked_add(retained_expression_nodes)
                        .ok_or(ModuleError::ExpressionLimit)?;
                }
                let retained_records = match &statement {
                    ModuleBodyStatement::Variables(variables) => variables.variables().len(),
                    ModuleBodyStatement::Declaration(_)
                    | ModuleBodyStatement::Assignment(_)
                    | ModuleBodyStatement::ApplicationAssignment(_)
                    | ModuleBodyStatement::ExpressionAssignment(_)
                    | ModuleBodyStatement::Call(_)
                    | ModuleBodyStatement::ExpressionCall(_) => 1,
                };
                if retained_records > remaining_records {
                    return Err(ModuleError::StatementLimit);
                }
                let maximum = remaining_records
                    .checked_add(procedure.statements.len())
                    .ok_or(ModuleError::StatementLimit)?;
                super::module_body::reserve(&mut procedure.statements, maximum)?;
                procedure.statements.push(statement);
                body_count = body_count
                    .checked_add(retained_records)
                    .ok_or(ModuleError::StatementLimit)?;
            }
            continue;
        }
        if !declaration_started {
            if let Some(name) = module_attribute(text, check).map_err(ModuleError::Cancelled)? {
                if colon_boundary || !attribute_header || inventory.module_name.is_some() {
                    return Err(ModuleError::Unsupported { range });
                }
                inventory.module_name = Some(range.start + name.start..range.start + name.end);
                continue;
            }
            if let Some(option) = module_option(text, check).map_err(ModuleError::Cancelled)? {
                if colon_boundary || options & option != 0 {
                    return Err(ModuleError::Unsupported { range });
                }
                options |= option;
                attribute_header = false;
                continue;
            }
        }
        if let Some(constant) =
            module_constant(text, range.clone(), check).map_err(ModuleError::Cancelled)?
        {
            if colon_boundary {
                return Err(ModuleError::Unsupported { range });
            }
            reserve_constants(
                &mut inventory.constants,
                limits.max_statements - body_count - inventory.variables.len(),
                check,
            )?;
            inventory.constants.push(constant);
            declaration_started = true;
            attribute_header = false;
            continue;
        }
        if module_variables(
            text,
            range.clone(),
            limits.max_statements - body_count - inventory.constants.len(),
            &mut inventory.variables,
            check,
        )? {
            if colon_boundary {
                return Err(ModuleError::Unsupported { range });
            }
            declaration_started = true;
            attribute_header = false;
            continue;
        }
        let remaining_parameters = limits
            .max_parameters
            .checked_sub(parameter_count)
            .ok_or(ModuleError::ParameterLimit)?;
        let Some(mut header) = closed_header(text, remaining_parameters, check)? else {
            return Err(ModuleError::Unsupported { range });
        };
        if inventory.procedures.len() >= limits.max_procedures {
            return Err(ModuleError::ProcedureLimit);
        }
        if inventory.procedures.len() == inventory.procedures.capacity() {
            let additional = inventory
                .procedures
                .capacity()
                .max(4)
                .min(limits.max_procedures - inventory.procedures.len());
            inventory
                .procedures
                .try_reserve_exact(additional)
                .map_err(|_| ModuleError::AllocationFailed)?;
        }
        parameter_count = parameter_count
            .checked_add(header.parameters.len())
            .ok_or(ModuleError::ParameterLimit)?;
        for parameter in &mut header.parameters {
            check().map_err(ModuleError::Cancelled)?;
            parameter.declaration =
                range.start + parameter.declaration.start..range.start + parameter.declaration.end;
            parameter.name = range.start + parameter.name.start..range.start + parameter.name.end;
            if let Some(ty) = &mut parameter.type_range {
                *ty = range.start + ty.start..range.start + ty.end;
            }
            if let Some(value) = &mut parameter.default_value {
                check().map_err(ModuleError::Cancelled)?;
                value.rebase(&range);
            }
        }
        open = Some(ModuleProcedure {
            kind: header.kind,
            declaration: range.clone(),
            name: range.start + header.name.start..range.start + header.name.end,
            parameters: header.parameters,
            header: range,
            body: cursor..cursor,
            terminator: cursor..cursor,
            statements: Vec::new(),
        });
        declaration_started = true;
        attribute_header = false;
    }
    if let Some(procedure) = open {
        return Err(ModuleError::Unsupported {
            range: procedure.declaration.start..source.len(),
        });
    }
    Ok(inventory)
}

struct ModuleHeader {
    kind: ProcedureKind,
    name: Range<usize>,
    parameters: Vec<ModuleParameter>,
}

fn closed_header<E>(
    text: &[u8],
    max_parameters: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleHeader>, ModuleError<E>> {
    let mut prefix = 0;
    if word(text, &mut prefix, check)
        .map_err(ModuleError::Cancelled)?
        .eq_ignore_ascii_case(b"Friend")
    {
        return Ok(None);
    }
    let Some(header) = procedure_header(text, check).map_err(ModuleError::Cancelled)? else {
        return Ok(None);
    };
    if !matches!(header.kind(), ProcedureKind::Sub | ProcedureKind::Function) {
        return Ok(None);
    }
    let name = text.get(header.name_span()).unwrap_or_default();
    if name
        .strip_suffix(b"!")
        .unwrap_or(name)
        .eq_ignore_ascii_case(b"Rem")
    {
        return Ok(None);
    }
    let mut cursor = header.name_span().end;
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)
        .map_err(ModuleError::Cancelled)?;
    let explicit_group = text.get(cursor) == Some(&b'(');
    let mut parameters = Vec::new();
    if explicit_group {
        cursor += 1;
        cursor += parameter_trivia_len(text.get(cursor..).unwrap_or_default(), check)
            .map_err(ModuleError::Cancelled)?;
        if text.get(cursor) != Some(&b')') {
            let Some(parsed) = scalar_parameters(text, &mut cursor, max_parameters, check)? else {
                return Ok(None);
            };
            parameters = parsed;
        }
        if text.get(cursor) != Some(&b')') {
            return Ok(None);
        }
        cursor += 1;
    }
    if header.kind() == ProcedureKind::Sub {
        cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)
            .map_err(ModuleError::Cancelled)?;
        return Ok((cursor == text.len()).then(|| ModuleHeader {
            kind: header.kind(),
            name: header.name_span(),
            parameters,
        }));
    }
    if !explicit_group {
        return Ok(None);
    }
    let suffix = text
        .get(header.name_span().end.saturating_sub(1))
        .is_some_and(|byte| b"%&^!#@$".contains(byte));
    let separator = logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)
        .map_err(ModuleError::Cancelled)?;
    cursor += separator;
    if cursor == text.len() {
        return Ok(Some(ModuleHeader {
            kind: header.kind(),
            name: header.name_span(),
            parameters,
        }));
    }
    if suffix || separator == 0 {
        return Ok(None);
    }
    let Some(ty) = super::scalar_return_type(text, check).map_err(ModuleError::Cancelled)? else {
        return Ok(None);
    };
    let spelling = text.get(ty.clone()).unwrap_or_default();
    if !super::module_body::intrinsic_type_name(spelling) {
        return Ok(None);
    }
    cursor = ty.end;
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)
        .map_err(ModuleError::Cancelled)?;
    Ok((cursor == text.len()).then(|| ModuleHeader {
        kind: header.kind(),
        name: header.name_span(),
        parameters,
    }))
}

fn scalar_parameters<E>(
    text: &[u8],
    cursor: &mut usize,
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Vec<ModuleParameter>>, ModuleError<E>> {
    let mut parameters = Vec::new();
    loop {
        check().map_err(ModuleError::Cancelled)?;
        if parameters.len() >= maximum {
            return Err(ModuleError::ParameterLimit);
        }
        let Some(parameter) =
            scalar_parameter(text, cursor, check).map_err(ModuleError::Cancelled)?
        else {
            return Ok(None);
        };
        reserve_parameters(&mut parameters, maximum, check)?;
        parameters.push(parameter);
        *cursor += parameter_trivia_len(text.get(*cursor..).unwrap_or_default(), check)
            .map_err(ModuleError::Cancelled)?;
        match text.get(*cursor) {
            Some(b')') => return Ok(Some(parameters)),
            Some(b',') => {
                *cursor += 1;
                *cursor += parameter_trivia_len(text.get(*cursor..).unwrap_or_default(), check)
                    .map_err(ModuleError::Cancelled)?;
                if matches!(text.get(*cursor), None | Some(b')' | b',')) {
                    return Ok(None);
                }
            }
            _ => return Ok(None),
        }
    }
}

fn reserve_parameters<E>(
    parameters: &mut Vec<ModuleParameter>,
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if parameters.len() >= maximum {
        return Err(ModuleError::ParameterLimit);
    }
    if parameters.len() == parameters.capacity() {
        let additional = parameters.capacity().max(4).min(maximum - parameters.len());
        reserve_parameters_exact(parameters, additional, check)?;
    }
    Ok(())
}

fn reserve_parameters_exact<E>(
    parameters: &mut Vec<ModuleParameter>,
    additional: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    let result = parameters
        .try_reserve_exact(additional)
        .map_err(|_| ModuleError::AllocationFailed);
    check().map_err(ModuleError::Cancelled)?;
    result
}

fn parameter_separator<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let length = parameter_trivia_len(text.get(*cursor..).unwrap_or_default(), check)?;
    *cursor += length;
    Ok(length != 0)
}

fn scalar_parameter<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleParameter>, E> {
    check()?;
    let start = *cursor;
    let is_optional = word(text, cursor, check)?.eq_ignore_ascii_case(b"Optional");
    if is_optional {
        if !parameter_separator(text, cursor, check)? {
            return Ok(None);
        }
    } else {
        *cursor = start;
    }
    let passing_start = *cursor;
    let modifier = word(text, cursor, check)?;
    let passing_mode = if modifier.eq_ignore_ascii_case(b"ByVal") {
        Some(ModuleParameterPassingMode::ByVal)
    } else if modifier.eq_ignore_ascii_case(b"ByRef") {
        Some(ModuleParameterPassingMode::ByRef)
    } else {
        *cursor = passing_start;
        None
    };
    if passing_mode.is_some() && !parameter_separator(text, cursor, check)? {
        return Ok(None);
    }
    let name_start = *cursor;
    let Some(spelling) = super::name(text, cursor, check)? else {
        return Ok(None);
    };
    // Modifiers are consumed only in their authored positions. The complete
    // As/type or list/default tail distinguishes names from extra modifiers.
    if text.get(name_start) != Some(&b'[')
        && [b"ParamArray".as_slice(), b"Rem"]
            .iter()
            .any(|keyword| spelling.eq_ignore_ascii_case(keyword))
    {
        return Ok(None);
    }
    let name = name_start..*cursor;
    let separator_length = parameter_trivia_len(text.get(*cursor..).unwrap_or_default(), check)?;
    *cursor += separator_length;
    let after_name = *cursor;
    let type_range = if word(text, cursor, check)?.eq_ignore_ascii_case(b"As") {
        if separator_length == 0 || !parameter_separator(text, cursor, check)? {
            return Ok(None);
        }
        let type_start = *cursor;
        if !super::module_body::intrinsic_type_name(word(text, cursor, check)?) {
            return Ok(None);
        }
        Some(type_start..*cursor)
    } else {
        *cursor = after_name;
        None
    };
    let declarator_end = type_range.as_ref().map_or(name.end, |ty| ty.end);
    *cursor += parameter_trivia_len(text.get(*cursor..).unwrap_or_default(), check)?;
    let default_value = if text.get(*cursor) == Some(&b'=') {
        if !is_optional {
            return Ok(None);
        }
        *cursor += 1;
        *cursor += parameter_trivia_len(text.get(*cursor..).unwrap_or_default(), check)?;
        let Some(value) = super::module_body::parameter_default_value(text, cursor, check)? else {
            return Ok(None);
        };
        Some(value)
    } else {
        None
    };
    let declaration_end = default_value
        .as_ref()
        .map_or(declarator_end, |value| value.span().end);
    Ok(Some(ModuleParameter {
        declaration: start..declaration_end,
        name,
        type_range,
        passing_mode,
        is_optional,
        default_value,
    }))
}

fn module_variables<E>(
    text: &[u8],
    range: Range<usize>,
    maximum: usize,
    variables: &mut Vec<ModuleVariable>,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    let mut cursor = 0;
    let prefix = word(text, &mut cursor, check).map_err(ModuleError::Cancelled)?;
    let visibility = if prefix.eq_ignore_ascii_case(b"Public") {
        Some(ModuleVariableVisibility::Public)
    } else if prefix.eq_ignore_ascii_case(b"Private") {
        Some(ModuleVariableVisibility::Private)
    } else if prefix.eq_ignore_ascii_case(b"Dim") {
        None
    } else {
        return Ok(false);
    };
    let visibility_range = visibility.map(|_| range.start..range.start + cursor);
    if !separator(text, &mut cursor, check).map_err(ModuleError::Cancelled)? {
        return Ok(false);
    }
    let original_len = variables.len();
    let result = (|| {
        loop {
            check().map_err(ModuleError::Cancelled)?;
            let Some(variable) = module_variable(
                text,
                &range,
                &mut cursor,
                visibility,
                visibility_range.clone(),
                check,
            )
            .map_err(ModuleError::Cancelled)?
            else {
                return Ok(false);
            };
            if cursor != text.len() && text.get(cursor) != Some(&b',') {
                return Ok(false);
            }
            reserve_variables(variables, maximum, check)?;
            variables.push(variable);
            if cursor == text.len() {
                return Ok(true);
            }
            cursor += 1;
            cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)
                .map_err(ModuleError::Cancelled)?;
        }
    })();
    if !matches!(&result, Ok(true)) {
        variables.truncate(original_len);
    }
    result
}

fn module_variable<E>(
    text: &[u8],
    range: &Range<usize>,
    cursor: &mut usize,
    visibility: Option<ModuleVariableVisibility>,
    visibility_range: Option<Range<usize>>,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleVariable>, E> {
    let Some(syntax) = variable_declarator(text, range, cursor, check)? else {
        return Ok(None);
    };
    Ok(Some(ModuleVariable {
        statement: range.clone(),
        declarator: syntax.declarator,
        name: syntax.name,
        array_bounds: syntax.array_bounds,
        array_work_nodes: syntax.array_work_nodes,
        type_range: syntax.type_range,
        visibility,
        visibility_range,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VariableDeclarator {
    pub(super) declarator: Range<usize>,
    pub(super) name: Range<usize>,
    pub(super) array_bounds: Option<Range<usize>>,
    pub(super) array_work_nodes: usize,
    pub(super) type_range: Option<Range<usize>>,
}

pub(super) fn variable_declarator<E>(
    text: &[u8],
    range: &Range<usize>,
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<VariableDeclarator>, E> {
    let Some(name) = super::module_body::simple_name(text, cursor, check)? else {
        return Ok(None);
    };
    let mut separator_length =
        logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
    *cursor += separator_length;
    let (array_bounds, array_work_nodes) = if text.get(*cursor) == Some(&b'(') {
        let Some(bounds) = numeric_array_bounds(text, cursor, check)? else {
            return Ok(None);
        };
        separator_length = logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
        *cursor += separator_length;
        (
            Some(range.start + bounds.span.start..range.start + bounds.span.end),
            bounds.work_nodes,
        )
    } else {
        (None, 0)
    };
    let after_declarator = *cursor;
    let type_range = if word(text, cursor, check)?.eq_ignore_ascii_case(b"As") {
        if (separator_length == 0 && array_bounds.is_none()) || !separator(text, cursor, check)? {
            return Ok(None);
        }
        let start = *cursor;
        if !super::module_body::intrinsic_type_name(word(text, cursor, check)?) {
            return Ok(None);
        }
        Some(range.start + start..range.start + *cursor)
    } else {
        *cursor = after_declarator;
        None
    };
    let declarator_end = type_range.as_ref().map_or_else(
        || {
            array_bounds
                .as_ref()
                .map_or(range.start + name.end, |bounds| bounds.end)
        },
        |ty| ty.end,
    );
    *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
    check()?;
    Ok(Some(VariableDeclarator {
        declarator: range.start + name.start..declarator_end,
        name: range.start + name.start..range.start + name.end,
        array_bounds,
        array_work_nodes,
        type_range,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NumericArrayBounds {
    span: Range<usize>,
    work_nodes: usize,
}

fn numeric_array_bounds<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<NumericArrayBounds>, E> {
    check()?;
    let start = *cursor;
    if text.get(*cursor) != Some(&b'(') {
        return Ok(None);
    }
    *cursor += 1;
    *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
    if text.get(*cursor) == Some(&b')') {
        *cursor += 1;
        return Ok(Some(NumericArrayBounds {
            span: start..*cursor,
            work_nodes: 1,
        }));
    }
    // Every counted component consumes at least one distinct authored byte.
    let mut work_nodes = 1_usize;
    loop {
        check()?;
        let Some(endpoint_nodes) = array_integer(text, cursor, check)? else {
            return Ok(None);
        };
        let Some(next_work) = work_nodes
            .checked_add(1)
            .and_then(|count| count.checked_add(endpoint_nodes))
        else {
            return Ok(None);
        };
        work_nodes = next_work;
        *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
        let after_endpoint = *cursor;
        if text
            .get(*cursor..)
            .unwrap_or_default()
            .get(..2)
            .is_some_and(|token| token.eq_ignore_ascii_case(b"To"))
        {
            *cursor += 2;
            *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
            let Some(endpoint_nodes) = array_integer(text, cursor, check)? else {
                return Ok(None);
            };
            let Some(next_work) = work_nodes.checked_add(endpoint_nodes) else {
                return Ok(None);
            };
            work_nodes = next_work;
            *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
        } else {
            *cursor = after_endpoint;
        }
        match text.get(*cursor) {
            Some(b')') => {
                *cursor += 1;
                return Ok(Some(NumericArrayBounds {
                    span: start..*cursor,
                    work_nodes,
                }));
            }
            Some(b',') => {
                *cursor += 1;
                *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
            }
            _ => return Ok(None),
        }
    }
}

fn array_integer<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<usize>, E> {
    check()?;
    let signed = matches!(text.get(*cursor), Some(b'+' | b'-'));
    if signed {
        *cursor += 1;
        *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)?;
    }
    let start = *cursor;
    while text.get(*cursor).is_some_and(u8::is_ascii_digit) {
        if cursor.is_multiple_of(512) {
            check()?;
        }
        *cursor += 1;
    }
    Ok((*cursor != start).then_some(1 + usize::from(signed)))
}

fn reserve_variables<E>(
    variables: &mut Vec<ModuleVariable>,
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if variables.len() >= maximum {
        return Err(ModuleError::StatementLimit);
    }
    if variables.len() == variables.capacity() {
        let additional = variables.capacity().max(1).min(maximum - variables.len());
        reserve_variables_exact(variables, additional, check)?;
    }
    Ok(())
}

fn reserve_variables_exact<E>(
    variables: &mut Vec<ModuleVariable>,
    additional: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    let result = variables
        .try_reserve_exact(additional)
        .map_err(|_| ModuleError::AllocationFailed);
    check().map_err(ModuleError::Cancelled)?;
    result
}

fn module_constant<E>(
    text: &[u8],
    range: Range<usize>,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleConstant>, E> {
    check()?;
    let mut cursor = 0;
    let first = word(text, &mut cursor, check)?;
    let visibility = if first.eq_ignore_ascii_case(b"Public") {
        Some(ModuleConstantVisibility::Public)
    } else if first.eq_ignore_ascii_case(b"Private") {
        Some(ModuleConstantVisibility::Private)
    } else {
        None
    };
    let visibility_range = visibility.map(|_| range.start..range.start + cursor);
    let keyword = if visibility.is_some() {
        if !separator(text, &mut cursor, check)? {
            return Ok(None);
        }
        word(text, &mut cursor, check)?
    } else {
        first
    };
    if !keyword.eq_ignore_ascii_case(b"Const") || !separator(text, &mut cursor, check)? {
        return Ok(None);
    }
    let Some(name) = super::module_body::simple_name(text, &mut cursor, check)? else {
        return Ok(None);
    };
    let separator_length = logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    cursor += separator_length;
    let after_name = cursor;
    let type_range = if word(text, &mut cursor, check)?.eq_ignore_ascii_case(b"As") {
        if separator_length == 0 || !separator(text, &mut cursor, check)? {
            return Ok(None);
        }
        let start = cursor;
        if !super::module_body::intrinsic_type_name(word(text, &mut cursor, check)?) {
            return Ok(None);
        }
        Some(range.start + start..range.start + cursor)
    } else {
        cursor = after_name;
        None
    };
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    if text.get(cursor) != Some(&b'=') {
        return Ok(None);
    }
    cursor += 1;
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    let Some(mut value) = super::module_body::literal_value(text, &mut cursor, check)? else {
        return Ok(None);
    };
    let declarator_end = value.span().end;
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    if cursor != text.len() {
        return Ok(None);
    }
    check()?;
    value.rebase(&range);
    Ok(Some(ModuleConstant {
        statement: range.clone(),
        declarator: range.start + name.start..range.start + declarator_end,
        name: range.start + name.start..range.start + name.end,
        type_range,
        value,
        visibility,
        visibility_range,
    }))
}

fn reserve_constants<E>(
    constants: &mut Vec<ModuleConstant>,
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    if constants.len() >= maximum {
        return Err(ModuleError::StatementLimit);
    }
    if constants.len() == constants.capacity() {
        let additional = constants.capacity().max(1).min(maximum - constants.len());
        reserve_constants_exact(constants, additional, check)?;
    }
    Ok(())
}

fn reserve_constants_exact<E>(
    constants: &mut Vec<ModuleConstant>,
    additional: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), ModuleError<E>> {
    check().map_err(ModuleError::Cancelled)?;
    let result = constants
        .try_reserve_exact(additional)
        .map_err(|_| ModuleError::AllocationFailed);
    check().map_err(ModuleError::Cancelled)?;
    result
}

fn is_terminator<E>(
    text: &[u8],
    kind: ProcedureKind,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let mut cursor = 0;
    if !word(text, &mut cursor, check)?.eq_ignore_ascii_case(b"End")
        || !separator(text, &mut cursor, check)?
    {
        return Ok(false);
    }
    let expected = if kind == ProcedureKind::Sub {
        b"Sub".as_slice()
    } else {
        b"Function".as_slice()
    };
    Ok(word(text, &mut cursor, check)?.eq_ignore_ascii_case(expected) && cursor == text.len())
}

fn module_attribute<E>(
    text: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Range<usize>>, E> {
    let mut cursor = 0;
    if !word(text, &mut cursor, check)?.eq_ignore_ascii_case(b"Attribute")
        || !separator(text, &mut cursor, check)?
        || !word(text, &mut cursor, check)?.eq_ignore_ascii_case(b"VB_Name")
    {
        return Ok(None);
    }
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    if text.get(cursor) != Some(&b'=') {
        return Ok(None);
    }
    cursor += 1;
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    if text.get(cursor) != Some(&b'"') {
        return Ok(None);
    }
    cursor += 1;
    let start = cursor;
    if !text.get(cursor).is_some_and(u8::is_ascii_alphabetic) {
        return Ok(None);
    }
    let _ = word(text, &mut cursor, check)?;
    let end = cursor;
    if end - start > 31 || text.get(cursor) != Some(&b'"') {
        return Ok(None);
    }
    cursor += 1;
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    Ok((cursor == text.len()).then_some(start..end))
}

fn supported_comment_tail<E>(
    tail: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let mut cursor = 0;
    while matches!(tail.get(cursor), Some(b' ' | b'\t')) {
        if cursor.is_multiple_of(512) {
            check()?;
        }
        cursor += 1;
    }
    let comment = if tail.get(cursor) == Some(&b'\'') {
        true
    } else if tail
        .get(cursor..cursor.saturating_add(3))
        .is_some_and(|keyword| keyword.eq_ignore_ascii_case(b"Rem"))
    {
        if !matches!(
            tail.get(cursor + 3),
            None | Some(b' ' | b'\t' | b'\r' | b'\n')
        ) {
            return Ok(false);
        }
        true
    } else {
        false
    };
    if !comment {
        return Ok(true);
    }
    let mut end = tail.len();
    if tail.get(end.saturating_sub(1)) == Some(&b'\n') {
        end -= 1;
        if tail.get(end.saturating_sub(1)) == Some(&b'\r') {
            end -= 1;
        }
    }
    while matches!(tail.get(end.wrapping_sub(1)), Some(b' ' | b'\t')) {
        if end.is_multiple_of(512) {
            check()?;
        }
        end -= 1;
    }
    // Comment continuations differ across classic dialects. Until their complete
    // lexical rule is implemented, no subsequent line may become accepted code.
    Ok(!(tail.get(end.wrapping_sub(1)) == Some(&b'_')
        && matches!(tail.get(end.wrapping_sub(2)), Some(b' ' | b'\t'))))
}

fn module_option<E>(
    text: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<u8>, E> {
    let mut cursor = 0;
    if !word(text, &mut cursor, check)?.eq_ignore_ascii_case(b"Option")
        || !separator(text, &mut cursor, check)?
    {
        return Ok(None);
    }
    let category = word(text, &mut cursor, check)?;
    let flag = if category.eq_ignore_ascii_case(b"Explicit") {
        1
    } else {
        if !separator(text, &mut cursor, check)? {
            return Ok(None);
        }
        let value = word(text, &mut cursor, check)?;
        if category.eq_ignore_ascii_case(b"Base") && matches!(value, b"0" | b"1") {
            2
        } else if category.eq_ignore_ascii_case(b"Compare")
            && [b"Binary".as_slice(), b"Text", b"Database"]
                .iter()
                .any(|expected| value.eq_ignore_ascii_case(expected))
        {
            4
        } else if category.eq_ignore_ascii_case(b"Private") && value.eq_ignore_ascii_case(b"Module")
        {
            8
        } else {
            return Ok(None);
        }
    };
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    Ok((cursor == text.len()).then_some(flag))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModuleScalarValueKind;

    fn parse(source: &[u8]) -> Result<ModuleInventory, ModuleError<()>> {
        module_inventory(
            source,
            ModuleLimits::new(source.len(), source.len(), source.len()),
            &mut || Ok(()),
        )
    }

    #[test]
    fn module_variables_preserve_exact_ranges_visibility_and_written_type_absence() {
        for newline in ["\n", "\r\n"] {
            let first = format!("pUbLiC _{newline} [value] _{newline} As _{newline} Long");
            let source = format!(
                "Attribute VB_Name = \"Variables\"{newline}Option Explicit{newline}  {first} ' tail{newline}Private [label]{newline}dIm implicit{newline}Dim written As Variant{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            assert!(inventory.procedures().is_empty());
            assert!(inventory.constants().is_empty());
            let variables = inventory.variables();
            assert_eq!(variables.len(), 4);
            assert_eq!(&source[variables[0].statement()], first);
            assert_eq!(
                &source[variables[0].declarator()],
                format!("[value] _{newline} As _{newline} Long")
            );
            assert_eq!(&source[variables[0].name()], "[value]");
            assert_eq!(&source[variables[0].type_range().unwrap()], "Long");
            assert_eq!(
                variables[0].visibility(),
                Some(ModuleVariableVisibility::Public)
            );
            assert_eq!(&source[variables[0].visibility_range().unwrap()], "pUbLiC");
            assert_eq!(&source[variables[1].statement()], "Private [label]");
            assert_eq!(&source[variables[1].declarator()], "[label]");
            assert_eq!(variables[1].name(), variables[1].declarator());
            assert!(variables[1].type_range().is_none());
            assert_eq!(
                variables[1].visibility(),
                Some(ModuleVariableVisibility::Private)
            );
            assert_eq!(&source[variables[1].visibility_range().unwrap()], "Private");
            assert_eq!(&source[variables[2].name()], "implicit");
            assert_eq!(variables[2].declarator(), variables[2].name());
            assert!(variables[2].type_range().is_none());
            assert_eq!(&source[variables[3].type_range().unwrap()], "Variant");
            for variable in &variables[2..] {
                assert_eq!(variable.visibility(), None);
                assert_eq!(variable.visibility_range(), None);
            }
            for variable in variables {
                assert_eq!(variable.name().start, variable.declarator().start);
                assert_eq!(
                    variable.declarator().end,
                    variable
                        .type_range()
                        .map_or(variable.name().end, |ty| ty.end)
                );
                assert!(variable.statement().start < variable.declarator().start);
                assert!(variable.declarator().end <= variable.statement().end);
            }
        }
    }

    #[test]
    fn module_variables_retain_all_intrinsic_types_brackets_and_contextual_names() {
        for prefix in ["Dim", "Public", "Private"] {
            for ty in [
                "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
                "Double", "Currency", "Date", "Variant", "Object",
            ] {
                let source = format!("{prefix} [value] As {ty}");
                let inventory = parse(source.as_bytes()).unwrap();
                assert_eq!(inventory.variables().len(), 1);
                assert_eq!(&source[inventory.variables()[0].type_range().unwrap()], ty);
                assert_eq!(&source[inventory.variables()[0].name()], "[value]");
            }
            for name in [
                "Name", "Line", "Load", "Unload", "Input", "Print", "Close", "Reset", "Beep",
                "DefBool", "_value", "Remember", "TrueRem", "[Me]", "[True]", "[Rem]", "[Sub]",
                "[Public]",
            ] {
                for tail in ["", " As Long"] {
                    let source = format!("{prefix} {name}{tail}");
                    let inventory = parse(source.as_bytes()).unwrap();
                    let variable = &inventory.variables()[0];
                    assert_eq!(&source[variable.name()], name);
                    assert_eq!(&source[variable.declarator()], format!("{name}{tail}"));
                    assert_eq!(variable.type_range().is_some(), !tail.is_empty());
                }
            }
        }
    }

    #[test]
    fn module_variables_surround_procedures_without_relabeling_local_records() {
        let source = b"Dim before As Long\nConst Seed=1\nSub First()\nDim local As Long\nEnd Sub\nPublic between\nFunction Second(Optional value=Seed) As Long\nSecond=value\nEnd Function\nPrivate after As Variant\nDim before\n";
        let inventory = parse(source).unwrap();
        assert_eq!(inventory.variables().len(), 4);
        assert_eq!(inventory.constants().len(), 1);
        assert_eq!(inventory.procedures().len(), 2);
        let first = &inventory.procedures()[0];
        assert!(
            matches!(&first.statements()[0], ModuleBodyStatement::Declaration(local) if &source[local.name()] == b"local")
        );
        assert_eq!(&source[inventory.variables()[0].name()], b"before");
        assert_eq!(&source[inventory.variables()[3].name()], b"before");
        assert!(inventory.variables()[0].statement().end < first.header().start);
        assert!(first.terminator().end < inventory.variables()[1].statement().start);
        assert!(
            inventory.procedures()[1].terminator().end < inventory.variables()[2].statement().start
        );
        for prefix in ["Dim value", "Public value As Long", "Private [value]"] {
            for tail in [
                "Option Explicit",
                "Option Base 1",
                "Attribute VB_Name = \"Late\"",
            ] {
                let source = format!("{prefix}\n{tail}\n");
                assert!(
                    matches!(
                        parse(source.as_bytes()),
                        Err(ModuleError::Unsupported { .. })
                    ),
                    "{source}"
                );
            }
        }
        for source in [
            "Dim value:",
            "Public value:Private other",
            ":Dim value",
            "Dim value\n:",
            "Sub First()\nEnd Sub:Dim value",
        ] {
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{source}"
            );
        }
        for body in ["Public value", "Private value As Long"] {
            let source = format!("Dim field\nSub Run()\n{body}\nEnd Sub\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn module_variables_reject_every_malformed_tail_without_publishing_prior_records() {
        for statement in [
            "Dim",
            "Public",
            "Private",
            "Dim value As",
            "Dim value As=1",
            "Dim value As Product",
            "Public value As [Long]",
            "Private value As Types.Result",
            "Dim value As New Object",
            "Dim value As Long()",
            "Dim value As Long$",
            "Public value As String * 8",
            "Dim value=1",
            "Public value As Long=1",
            "Private value:=1",
            "Dim value==1",
            "Dim value,other As Product",
            "Public value As Long,other As",
            "Private value,",
            "Dim value$",
            "Dim value!",
            "Public [value]$",
            "Dim value(upper)",
            "Dim value(1+2)",
            "Dim holder.value",
            "Dim holder!value",
            "Dim value extra",
            "Public value As Long extra",
            "Dim [value]As Long",
            "Dim [value",
            "Dim []",
            "Dim [value name]",
            "Dim é",
            "Public Private value",
            "Private Public value",
            "Public Dim value",
            "Private Dim value",
            "Dim Public value",
            "Dim Private value",
            "Public Static value",
            "Static value",
            "Global value",
            "Friend value",
            "Public WithEvents value As Object",
            "DimWith value",
            "Dim value Rem tail",
            "Private Rem",
            "Dim value\0",
            "Dim value\r extra",
        ] {
            let source = format!(
                "Dim prior\nConst Seed=1\nSub Accepted()\nDim local As Long\nEnd Sub\n{statement}\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{statement:?}"
            );
        }
        let source = b"Dim value\r\n";
        let inventory = parse(source).unwrap();
        assert_eq!(inventory.statement_count(), 1);
        assert_eq!(inventory.variables().len(), 1);
        let variable = &inventory.variables()[0];
        assert_eq!(&source[variable.statement()], b"Dim value");
        assert_eq!(&source[variable.declarator()], b"value");
        assert!(variable.type_range().is_none());
    }

    #[test]
    fn module_variables_share_statement_quota_with_constants_and_body_records() {
        let source = b"Dim first\nPublic second As Variant\nPrivate third\n";
        let limits = ModuleLimits::new(source.len(), 3, 0)
            .with_max_parameters(0)
            .with_max_arguments(0);
        let inventory = module_inventory(source, limits, &mut || Ok::<(), ()>(())).unwrap();
        assert_eq!(inventory.variables().len(), 3);
        assert_eq!(inventory.statement_count(), 3);
        assert!(inventory.procedures().is_empty());
        for maximum in [0, 1, 2] {
            assert_eq!(
                module_inventory(
                    source,
                    ModuleLimits::new(source.len(), maximum, 0),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::StatementLimit)
            );
        }
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len() - 1, 3, 0),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::SourceLimit)
        );
        let source = b"Dim first\nConst Seed=1\nSub Run()\nDim local As Long\nlocal=Seed\nEnd Sub\nPublic last As Long\nConst Final=2\n";
        let limits = ModuleLimits::new(source.len(), 8, 1);
        let inventory = module_inventory(source, limits, &mut || Ok::<(), ()>(())).unwrap();
        assert_eq!(inventory.statement_count(), 8);
        assert_eq!(inventory.variables().len(), 2);
        assert_eq!(inventory.constants().len(), 2);
        assert_eq!(inventory.procedures()[0].statements().len(), 2);
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 7, 1), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::StatementLimit)
        );
        let mut many = String::new();
        for index in 0..128 {
            many.push_str(&format!("Dim value{index}\n"));
        }
        let inventory = module_inventory(
            many.as_bytes(),
            ModuleLimits::new(many.len(), 128, 0),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(inventory.variables().len(), 128);
        for (index, variable) in inventory.variables().iter().enumerate() {
            assert_eq!(&many[variable.name()], format!("value{index}"));
        }
        assert_eq!(
            module_inventory(
                many.as_bytes(),
                ModuleLimits::new(many.len(), 127, 0),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::StatementLimit)
        );
    }

    #[test]
    fn module_variable_retention_propagates_allocation_and_cancellation_errors() {
        let source = b"Private [value] As Long";
        let mut parsed = Vec::new();
        assert!(
            module_variables(
                source,
                0..source.len(),
                1,
                &mut parsed,
                &mut || Ok::<(), ()>(())
            )
            .unwrap()
        );
        let variable = parsed.pop().unwrap();
        let mut variables = Vec::new();
        assert_eq!(
            reserve_variables(&mut variables, 0, &mut || Ok::<(), ()>(())),
            Err(ModuleError::StatementLimit)
        );
        reserve_variables(&mut variables, 1, &mut || Ok::<(), ()>(())).unwrap();
        variables.push(variable.clone());
        assert_eq!(
            reserve_variables(&mut variables, 1, &mut || Ok::<(), ()>(())),
            Err(ModuleError::StatementLimit)
        );
        assert_eq!(
            reserve_variables_exact(&mut variables, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert_eq!(variables, vec![variable]);
        for cancelled_at in [1, 2] {
            let mut empty = Vec::new();
            let mut calls = 0;
            assert_eq!(
                reserve_variables_exact(&mut empty, 1, &mut || {
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
        }
    }

    #[test]
    fn module_variable_scans_cancel_at_every_checkpoint_without_partial_inventory() {
        let source = format!(
            "Dim first\nPrivate _\r\n [{}] _\n As _\r\n Long\nConst Seed=1\nSub Run()\nDim local As Long\nEnd Sub\nPublic [{}]\n",
            "x".repeat(1024),
            "y".repeat(1024)
        );
        let limits = ModuleLimits::new(source.len(), 7, 1);
        let mut successful_checks = 0;
        let inventory = module_inventory(source.as_bytes(), limits, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(inventory.variables().len(), 3);
        assert_eq!(inventory.constants().len(), 1);
        assert_eq!(inventory.procedures()[0].statements().len(), 1);
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source.as_bytes(), limits, &mut || {
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
    fn module_variable_all_byte_boundaries_require_complete_original_tokens() {
        for byte in 0..=u8::MAX {
            for (prefix, suffix, accepted) in [
                (
                    b"Dim".as_slice(),
                    b"[value]".as_slice(),
                    matches!(byte, b' ' | b'\t'),
                ),
                (
                    b"Dim [value]".as_slice(),
                    b"As Long".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'\''),
                ),
                (
                    b"Dim [value] As".as_slice(),
                    b"Long".as_slice(),
                    matches!(byte, b' ' | b'\t'),
                ),
                (
                    b"Dim [value]".as_slice(),
                    b"".as_slice(),
                    byte.is_ascii_whitespace() && byte != b'\r' || byte == b'\'',
                ),
                (
                    b"Public [value] As Long".as_slice(),
                    b"".as_slice(),
                    byte.is_ascii_whitespace() && byte != b'\r' || byte == b'\'',
                ),
                (
                    b"Private ".as_slice(),
                    b"".as_slice(),
                    byte.is_ascii_alphabetic() || byte == b'_',
                ),
                (
                    b"Dim [".as_slice(),
                    b"]".as_slice(),
                    byte.is_ascii_alphabetic() || byte == b'_',
                ),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let result = parse(&source);
                assert_eq!(
                    result.is_ok(),
                    accepted,
                    "boundary byte {byte}, prefix {prefix:?}"
                );
                if let Ok(inventory) = result {
                    assert_eq!(inventory.variables().len(), 1);
                    let variable = &inventory.variables()[0];
                    assert!(variable.name().start < variable.name().end);
                    assert!(variable.declarator().end <= variable.statement().end);
                    assert!(variable.statement().end <= source.len());
                    if variable.type_range().is_none() {
                        assert_eq!(variable.declarator(), variable.name());
                    }
                    if byte == b'\'' && prefix == b"Dim [value]" && suffix == b"As Long" {
                        assert_eq!(&source[variable.statement()], b"Dim [value]");
                        assert_eq!(&source[variable.declarator()], b"[value]");
                        assert!(variable.type_range().is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn grouped_module_variables_preserve_independent_ranges_and_shared_statement() {
        for newline in ["\n", "\r\n"] {
            let clauses = [
                "[implicit]".to_owned(),
                format!("[typed] _{newline} As _{newline} Long"),
                "pending()".to_owned(),
                format!("[matrix] _{newline} ( -1 To _{newline} +2, 003 )As Variant"),
                "last".to_owned(),
            ];
            let statement = format!(
                "pUbLiC _{newline} {}",
                clauses.join(&format!(" _{newline} , _{newline} "))
            );
            let source = format!(
                "Attribute VB_Name = \"Groups\"{newline}Option Explicit{newline}  {statement} ' tail{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(inventory.statement_count(), 3);
            assert_eq!(inventory.variables().len(), clauses.len());
            assert!(inventory.procedures().is_empty());
            assert!(inventory.constants().is_empty());
            let first = &inventory.variables()[0];
            for (variable, clause) in inventory.variables().iter().zip(&clauses) {
                assert_eq!(&source[variable.statement()], statement);
                assert_eq!(variable.statement(), first.statement());
                assert_eq!(&source[variable.declarator()], clause);
                assert_eq!(
                    variable.visibility(),
                    Some(ModuleVariableVisibility::Public)
                );
                assert_eq!(variable.visibility_range(), first.visibility_range());
                assert_eq!(&source[variable.visibility_range().unwrap()], "pUbLiC");
                assert_eq!(variable.name().start, variable.declarator().start);
                assert!(variable.declarator().end <= variable.statement().end);
            }
            for index in [0, 2, 4] {
                assert!(inventory.variables()[index].type_range().is_none());
            }
            assert_eq!(
                &source[inventory.variables()[1].type_range().unwrap()],
                "Long"
            );
            assert_eq!(
                &source[inventory.variables()[3].type_range().unwrap()],
                "Variant"
            );
            assert_eq!(
                &source[inventory.variables()[2].array_bounds().unwrap()],
                "()"
            );
            assert_eq!(
                &source[inventory.variables()[3].array_bounds().unwrap()],
                format!("( -1 To _{newline} +2, 003 )")
            );
            assert_eq!(inventory.variables()[2].array_work_nodes(), 1);
            assert_eq!(inventory.variables()[3].array_work_nodes(), 8);
            for index in [0, 1, 4] {
                assert!(inventory.variables()[index].array_bounds().is_none());
                assert_eq!(inventory.variables()[index].array_work_nodes(), 0);
            }
            for pair in inventory.variables().windows(2) {
                assert!(pair[0].declarator().end < pair[1].declarator().start);
            }
        }
    }

    #[test]
    fn grouped_module_variables_keep_per_member_types_visibility_and_contextual_names() {
        for prefix in ["Dim", "Public", "Private"] {
            for ty in [
                "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
                "Double", "Currency", "Date", "Variant", "Object",
            ] {
                let source = format!(
                    "{prefix} absent, [typed] As {ty}, pending(), [matrix](1,2)As {ty}, last"
                );
                let inventory = parse(source.as_bytes()).unwrap();
                assert_eq!(inventory.statement_count(), 1);
                assert_eq!(inventory.variables().len(), 5);
                for index in [0, 2, 4] {
                    assert!(
                        inventory.variables()[index].type_range().is_none(),
                        "{source}"
                    );
                }
                assert_eq!(&source[inventory.variables()[2].name()], "pending");
                assert_eq!(
                    &source[inventory.variables()[2].array_bounds().unwrap()],
                    "()"
                );
                assert_eq!(inventory.variables()[2].array_work_nodes(), 1);
                for index in [1, 3] {
                    assert_eq!(
                        &source[inventory.variables()[index].type_range().unwrap()],
                        ty
                    );
                }
                for variable in inventory.variables() {
                    let expected = match prefix {
                        "Public" => Some(ModuleVariableVisibility::Public),
                        "Private" => Some(ModuleVariableVisibility::Private),
                        _ => None,
                    };
                    assert_eq!(variable.visibility(), expected);
                    assert_eq!(variable.visibility_range().is_some(), expected.is_some());
                    assert_eq!(&source[variable.statement()], source);
                }
            }
            let names = [
                "Name", "Line", "Load", "Unload", "Input", "Print", "Close", "Reset", "Beep",
                "DefBool", "_value", "Remember", "TrueRem", "[Me]", "[True]", "[Rem]", "[Sub]",
                "[Public]",
            ];
            let source = format!("{prefix} {}", names.join(","));
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(inventory.variables().len(), names.len());
            assert_eq!(inventory.statement_count(), 1);
            for (variable, name) in inventory.variables().iter().zip(names) {
                assert_eq!(&source[variable.name()], name);
                assert_eq!(variable.name(), variable.declarator());
                assert!(variable.type_range().is_none());
            }
        }
    }

    #[test]
    fn grouped_module_variables_keep_duplicates_and_module_order_without_body_expansion() {
        let source = b"Dim before, before As Variant\nConst Seed=1\nSub First()\nDim local As Long\nEnd Sub\nPublic between(1,2), plain\nFunction Second(Optional value=Seed) As Long\nSecond=value\nEnd Function\nPrivate after(), before\n";
        let inventory = parse(source).unwrap();
        assert_eq!(inventory.variables().len(), 6);
        assert_eq!(inventory.constants().len(), 1);
        assert_eq!(inventory.procedures().len(), 2);
        assert_eq!(
            inventory.variables()[0].statement(),
            inventory.variables()[1].statement()
        );
        assert_eq!(
            inventory.variables()[2].statement(),
            inventory.variables()[3].statement()
        );
        assert_eq!(
            inventory.variables()[4].statement(),
            inventory.variables()[5].statement()
        );
        assert_eq!(&source[inventory.variables()[0].name()], b"before");
        assert_eq!(&source[inventory.variables()[1].name()], b"before");
        assert_eq!(&source[inventory.variables()[5].name()], b"before");
        assert_ne!(
            inventory.variables()[0].name(),
            inventory.variables()[1].name()
        );
        assert!(
            inventory.variables()[1].declarator().end < inventory.procedures()[0].header().start
        );
        assert!(
            inventory.procedures()[0].terminator().end
                < inventory.variables()[2].declarator().start
        );
        assert!(
            inventory.variables()[3].declarator().end < inventory.procedures()[1].header().start
        );
        assert!(
            inventory.procedures()[1].terminator().end
                < inventory.variables()[4].declarator().start
        );
        assert!(
            matches!(&inventory.procedures()[0].statements()[0], ModuleBodyStatement::Declaration(local) if &source[local.name()] == b"local")
        );
        for source in [
            "Dim first,second\nOption Explicit",
            "Public first,second\nAttribute VB_Name = \"Late\"",
            "Dim first,second:",
            "Dim first,second:Private third",
            "Const first=1,second=2",
        ] {
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn grouped_module_variables_reject_incomplete_and_richer_later_members_atomically() {
        for tail in [
            "",
            ",second",
            "second,",
            "second,,third",
            "second, ,third",
            "second As",
            "second As Product",
            "second As New Object",
            "second As String * 8",
            "second As Long()",
            "second As Long$",
            "second$",
            "[second]$",
            "second!",
            "second=1",
            "second As Long=1",
            "second:=1",
            "second==1",
            "holder.second",
            "holder!second",
            "second extra",
            "second(upper)",
            "second(1+2)",
            "second(,1)",
            "second(1,)",
            "second(1,,2)",
            "second(1 To)",
            "second(1",
            "second(1)(2)",
            "second(1) As",
            "second(1) As Product",
            "second(1) = 1",
            "second(1),third As",
            "[second",
            "[]",
            "[second name]",
            "é",
            "Dim second",
            "Public second",
            "Private second",
            "Static second",
            "WithEvents second As Object",
            "Empty",
            "eMpTy()",
            "Empty As Long",
            "Rem tail",
            "second Rem tail",
            "second\0",
            "second\r extra",
            "second\nthird",
        ] {
            let source = format!(
                "Dim prior\nConst Seed=1\nSub Accepted()\nEnd Sub\nPrivate first(-1 To 2,3) As Long, {tail}\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{tail:?}"
            );
        }
        let preserved = parse(b"Dim prior").unwrap().variables()[0].clone();
        for source in [
            b"Dim first,second As".as_slice(),
            b"Dim first(1,2),second(upper)",
            b"Dim first,",
        ] {
            let mut variables = vec![preserved.clone()];
            assert!(
                !module_variables(source, 0..source.len(), 8, &mut variables, &mut || Ok::<
                    (),
                    (),
                >(
                    ()
                ))
                .unwrap()
            );
            assert_eq!(variables, vec![preserved.clone()]);
        }
    }

    #[test]
    fn grouped_module_variables_obey_shared_record_quota_independently_of_statement_count() {
        let source = b"Dim first,second(1,2),third As Long,fourth(),fifth";
        let limits = ModuleLimits::new(source.len(), 5, 0)
            .with_max_parameters(0)
            .with_max_arguments(0);
        let inventory = module_inventory(source, limits, &mut || Ok::<(), ()>(())).unwrap();
        assert_eq!(inventory.statement_count(), 1);
        assert_eq!(inventory.variables().len(), 5);
        assert!(inventory.procedures().is_empty());
        for maximum in 0..5 {
            assert_eq!(
                module_inventory(
                    source,
                    ModuleLimits::new(source.len(), maximum, 0),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::StatementLimit)
            );
        }
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len() - 1, 5, 0),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::SourceLimit)
        );
        let source = b"Dim first,second,third\nConst Seed=1\nSub Run()\nDim local As Long\nEnd Sub\nPrivate fourth(),fifth(1),sixth As Long\n";
        let inventory =
            module_inventory(source, ModuleLimits::new(source.len(), 8, 1), &mut || {
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_eq!(inventory.statement_count(), 6);
        assert_eq!(inventory.variables().len(), 6);
        assert_eq!(inventory.constants().len(), 1);
        assert_eq!(inventory.procedures()[0].statements().len(), 1);
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 7, 1), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::StatementLimit)
        );
        for source in [
            "Const Seed=1\nDim first,second,third",
            "Dim first,second,third\nConst Seed=1",
            "Sub Run()\nDim local As Long\nEnd Sub\nDim first,second,third",
            "Dim first,second,third\nSub Run()\nDim local As Long\nEnd Sub",
        ] {
            let required = 4;
            let inventory = module_inventory(
                source.as_bytes(),
                ModuleLimits::new(source.len(), required, 1),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap();
            assert_eq!(inventory.variables().len(), 3);
            assert_eq!(
                module_inventory(
                    source.as_bytes(),
                    ModuleLimits::new(source.len(), required - 1, 1),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::StatementLimit)
            );
        }
    }

    #[test]
    fn grouped_module_variables_large_lists_keep_source_order_and_fallible_retention() {
        let declarations = (0..128)
            .map(|index| match index % 3 {
                0 => format!("[value{index}]"),
                1 => format!("[value{index}](-1 To +2,{index})"),
                _ => format!("[value{index}] As Long"),
            })
            .collect::<Vec<_>>();
        let source = format!("Private {}", declarations.join(","));
        let inventory = module_inventory(
            source.as_bytes(),
            ModuleLimits::new(source.len(), 128, 0),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(inventory.statement_count(), 1);
        assert_eq!(inventory.variables().len(), declarations.len());
        for (index, (variable, declaration)) in
            inventory.variables().iter().zip(&declarations).enumerate()
        {
            assert_eq!(&source[variable.declarator()], declaration);
            assert_eq!(&source[variable.name()], format!("[value{index}]"));
            assert_eq!(variable.statement(), inventory.variables()[0].statement());
        }
        assert_eq!(
            module_inventory(
                source.as_bytes(),
                ModuleLimits::new(source.len(), 127, 0),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::StatementLimit)
        );
        let preserved = parse(b"Dim prior").unwrap().variables()[0].clone();
        let source = b"Dim first,second";
        let mut variables = vec![preserved.clone()];
        assert_eq!(
            module_variables(source, 0..source.len(), 2, &mut variables, &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::StatementLimit)
        );
        assert_eq!(variables, vec![preserved.clone()]);
        assert_eq!(
            reserve_variables_exact(&mut variables, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert_eq!(variables, vec![preserved]);
    }

    #[test]
    fn grouped_module_variables_cancel_at_every_checkpoint_without_exposing_prefixes() {
        let source = format!(
            "Dim prior\nPublic _\r\n [{}] _\n , _\r\n [matrix]( -{} To +{}, 3) _\n As Long, _\r\n last()\nConst Seed=1\nSub Run()\nDim local As Long\nEnd Sub\n",
            "x".repeat(1024),
            "9".repeat(1024),
            "8".repeat(1024)
        );
        let limits = ModuleLimits::new(source.len(), 8, 1);
        let mut checkpoints = 0;
        let inventory = module_inventory(source.as_bytes(), limits, &mut || {
            checkpoints += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(inventory.variables().len(), 4);
        for cancelled_at in 1..=checkpoints {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source.as_bytes(), limits, &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err(ModuleError::Cancelled("stop")),
                "module checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
        let source = b"Private first(1,2),second As Long,third()";
        let preserved = parse(b"Dim prior").unwrap().variables()[0].clone();
        let mut variables = vec![preserved.clone()];
        let mut checkpoints = 0;
        assert!(
            module_variables(source, 0..source.len(), 4, &mut variables, &mut || {
                checkpoints += 1;
                Ok::<(), ()>(())
            })
            .unwrap()
        );
        assert_eq!(variables.len(), 4);
        for cancelled_at in 1..=checkpoints {
            let mut variables = vec![preserved.clone()];
            let mut calls = 0;
            assert_eq!(
                module_variables(source, 0..source.len(), 4, &mut variables, &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err(ModuleError::Cancelled("stop")),
                "group checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
            assert_eq!(variables, vec![preserved.clone()]);
        }
    }

    #[test]
    fn grouped_module_variable_all_byte_outer_separators_preserve_complete_lists() {
        for byte in 0..=u8::MAX {
            for prefix in [
                "Dim [first]",
                "Public [first](0,1)",
                "Private [first] As Long",
            ] {
                let source = [prefix.as_bytes(), &[byte], b"[second]"].concat();
                let result = parse(&source);
                assert_eq!(
                    result.is_ok(),
                    matches!(byte, b',' | b'\''),
                    "outer byte {byte}, prefix {prefix}"
                );
                if let Ok(inventory) = result {
                    assert_eq!(
                        inventory.variables().len(),
                        if byte == b',' { 2 } else { 1 }
                    );
                    assert_eq!(inventory.statement_count(), 1);
                    assert_eq!(&source[inventory.variables()[0].name()], b"[first]");
                    if byte == b',' {
                        assert_eq!(&source[inventory.variables()[1].name()], b"[second]");
                        assert_eq!(
                            inventory.variables()[0].statement(),
                            inventory.variables()[1].statement()
                        );
                    } else {
                        assert_eq!(inventory.variables()[0].statement().end, prefix.len());
                    }
                }
            }
            for (prefix, suffix, accepted) in [
                (
                    b"Dim [first],".as_slice(),
                    b"[second]".as_slice(),
                    matches!(byte, b' ' | b'\t'),
                ),
                (
                    b"Dim [first]".as_slice(),
                    b",[second]".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'\''),
                ),
                (
                    b"Dim [first],[second]".as_slice(),
                    b"".as_slice(),
                    byte.is_ascii_whitespace() && byte != b'\r' || byte == b'\'',
                ),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let result = parse(&source);
                assert_eq!(
                    result.is_ok(),
                    accepted,
                    "comma trivia byte {byte}, prefix {prefix:?}"
                );
                if let Ok(inventory) = result {
                    let expected = if byte == b'\'' && suffix == b",[second]" {
                        1
                    } else {
                        2
                    };
                    assert_eq!(inventory.variables().len(), expected);
                    for variable in inventory.variables() {
                        assert!(variable.declarator().end <= variable.statement().end);
                        assert!(variable.statement().end <= source.len());
                    }
                }
            }
        }
    }

    #[test]
    fn module_array_variables_preserve_bounds_ranges_and_scalar_absence() {
        for newline in ["\n", "\r\n"] {
            let bounds =
                format!("( _{newline} - _{newline} 2 To _{newline} +3, 9, -0 To 0004 _{newline} )");
            let declaration = format!("[values] _{newline} {bounds} _{newline} As _{newline} Long");
            let first = format!("pUbLiC _{newline} {declaration}");
            let source = format!(
                "Attribute VB_Name = \"Arrays\"{newline}Option Base 1{newline}  {first} ' tail{newline}Private pending(){newline}Dim fixed(001){newline}Dim scalar{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            assert!(inventory.procedures().is_empty());
            assert!(inventory.constants().is_empty());
            let variables = inventory.variables();
            assert_eq!(variables.len(), 4);
            assert_eq!(&source[variables[0].statement()], first);
            assert_eq!(&source[variables[0].declarator()], declaration);
            assert_eq!(&source[variables[0].name()], "[values]");
            assert_eq!(&source[variables[0].array_bounds().unwrap()], bounds);
            assert_eq!(variables[0].array_work_nodes(), 12);
            assert_eq!(&source[variables[0].type_range().unwrap()], "Long");
            assert_eq!(
                variables[0].visibility(),
                Some(ModuleVariableVisibility::Public)
            );
            assert_eq!(&source[variables[0].visibility_range().unwrap()], "pUbLiC");
            assert_eq!(&source[variables[1].array_bounds().unwrap()], "()");
            assert_eq!(variables[1].array_work_nodes(), 1);
            assert_eq!(&source[variables[1].declarator()], "pending()");
            assert_eq!(
                variables[1].visibility(),
                Some(ModuleVariableVisibility::Private)
            );
            assert!(variables[1].type_range().is_none());
            assert_eq!(&source[variables[2].array_bounds().unwrap()], "(001)");
            assert_eq!(variables[2].array_work_nodes(), 3);
            assert_eq!(&source[variables[2].declarator()], "fixed(001)");
            assert!(variables[2].type_range().is_none());
            assert_eq!(variables[2].visibility(), None);
            assert_eq!(variables[2].visibility_range(), None);
            assert!(variables[3].array_bounds().is_none());
            assert_eq!(variables[3].array_work_nodes(), 0);
            assert_eq!(variables[3].declarator(), variables[3].name());
            for variable in variables {
                if let Some(bounds) = variable.array_bounds() {
                    assert_eq!(source.as_bytes()[bounds.start], b'(');
                    assert_eq!(source.as_bytes()[bounds.end - 1], b')');
                    assert!(variable.name().end <= bounds.start);
                    assert!(bounds.end <= variable.declarator().end);
                    if variable.type_range().is_none() {
                        assert_eq!(variable.declarator().end, bounds.end);
                    }
                }
                assert_eq!(variable.name().start, variable.declarator().start);
                assert!(variable.declarator().end <= variable.statement().end);
            }
        }
    }

    #[test]
    fn module_array_variables_retain_complete_numeric_dimension_lists() {
        for prefix in ["Dim", "Public", "Private"] {
            for bounds in [
                "()",
                "( )",
                "(0)",
                "(+0)",
                "(-1)",
                "(+ 12)",
                "(-\t12)",
                "(0 To 9)",
                "(-12 tO + 24)",
                "(9 To -9)",
                "(1To -2)",
                "(1 To-2)",
                "(1To+2)",
                "(1To2)",
                "(1,2)",
                "(1, -2 To +3, +4, 0 To 0)",
                "( _\n 1 _\r\n To _\n - _\r\n 2, _\n +3 _\r\n )",
            ] {
                for tail in ["", " As Variant"] {
                    let source = format!("{prefix} [value] {bounds}{tail}\n");
                    let inventory = parse(source.as_bytes()).unwrap();
                    assert_eq!(inventory.variables().len(), 1, "{source:?}");
                    let variable = &inventory.variables()[0];
                    assert_eq!(&source[variable.array_bounds().unwrap()], bounds);
                    assert_eq!(&source[variable.name()], "[value]");
                    assert_eq!(
                        &source[variable.declarator()],
                        format!("[value] {bounds}{tail}")
                    );
                    assert_eq!(variable.type_range().is_some(), !tail.is_empty());
                }
            }
            let source = format!("{prefix} [value](1)As Long");
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(
                &source[inventory.variables()[0].declarator()],
                "[value](1)As Long"
            );
            assert_eq!(
                &source[inventory.variables()[0].type_range().unwrap()],
                "Long"
            );
            for ty in [
                "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
                "Double", "Currency", "Date", "Variant", "Object",
            ] {
                let source = format!("{prefix} [value](2,-1 To 4) As {ty}");
                let inventory = parse(source.as_bytes()).unwrap();
                assert_eq!(&source[inventory.variables()[0].type_range().unwrap()], ty);
                assert_eq!(
                    &source[inventory.variables()[0].array_bounds().unwrap()],
                    "(2,-1 To 4)"
                );
            }
            for name in [
                "Name", "Line", "Load", "Remember", "_values", "[True]", "[Rem]", "[Sub]",
            ] {
                let source = format!("{prefix} {name}(1)");
                let inventory = parse(source.as_bytes()).unwrap();
                assert_eq!(&source[inventory.variables()[0].name()], name);
            }
        }
    }

    #[test]
    fn module_array_variables_charge_groups_dimensions_endpoints_and_written_signs() {
        for (bounds, expected) in [
            ("()", 1),
            ("(0)", 3),
            ("(+0)", 4),
            ("(- 0)", 4),
            ("(0 To 1)", 4),
            ("(-2 To +3,4)", 8),
            ("(1,2,3)", 7),
            ("(- _\n 1 To + _\r\n 2, +3 To -4)", 11),
        ] {
            let source = format!("Dim value{bounds}");
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(
                inventory.variables()[0].array_work_nodes(),
                expected,
                "{bounds:?}"
            );
            let mut cursor = 0;
            let scanned =
                numeric_array_bounds(bounds.as_bytes(), &mut cursor, &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .unwrap();
            assert_eq!(scanned.work_nodes, expected);
            assert_eq!(scanned.span, 0..bounds.len());
            assert!(scanned.work_nodes <= bounds.len());
        }
        let inventory = parse(b"Dim scalar").unwrap();
        assert_eq!(inventory.variables()[0].array_work_nodes(), 0);
    }

    #[test]
    fn module_array_variables_do_not_evaluate_magnitudes_or_retain_dimensions() {
        let magnitude = "9".repeat(4096);
        let bounds = format!("(-{magnitude} To +{magnitude})");
        let source = format!("Private wide{bounds}\n");
        let inventory = module_inventory(
            source.as_bytes(),
            ModuleLimits::new(source.len(), 1, 0),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(inventory.statement_count(), 1);
        assert_eq!(inventory.variables().len(), 1);
        assert_eq!(
            &source[inventory.variables()[0].array_bounds().unwrap()],
            bounds
        );
        assert_eq!(inventory.variables()[0].array_work_nodes(), 6);
        let dimensions = (0..128)
            .map(|index| format!("-{index} To +{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("Dim many({dimensions})");
        let inventory = module_inventory(
            source.as_bytes(),
            ModuleLimits::new(source.len(), 1, 0),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(inventory.statement_count(), 1);
        assert_eq!(inventory.variables().len(), 1);
        assert_eq!(
            &source[inventory.variables()[0].array_bounds().unwrap()],
            format!("({dimensions})")
        );
        assert_eq!(inventory.variables()[0].array_work_nodes(), 1 + 128 * 5);
        assert_eq!(inventory.variables()[0].declarator().end, source.len());
    }

    #[test]
    fn module_array_variables_preserve_module_order_and_other_closed_scopes() {
        let source = b"Dim before()\nConst Seed=1\nSub First()\nDim local As Long\nEnd Sub\nPublic between(1,2) As Long\nFunction Second(Optional value=Seed) As Long\nSecond=value\nEnd Function\nPrivate after(-1 To 1)\n";
        let inventory = parse(source).unwrap();
        assert_eq!(inventory.variables().len(), 3);
        assert_eq!(inventory.constants().len(), 1);
        assert_eq!(inventory.procedures().len(), 2);
        assert!(
            inventory.variables()[0].statement().end < inventory.procedures()[0].header().start
        );
        assert!(
            inventory.procedures()[0].terminator().end < inventory.variables()[1].statement().start
        );
        assert!(
            inventory.variables()[1].statement().end < inventory.procedures()[1].header().start
        );
        assert!(
            inventory.procedures()[1].terminator().end < inventory.variables()[2].statement().start
        );
        assert!(
            matches!(&inventory.procedures()[0].statements()[0], ModuleBodyStatement::Declaration(local) if &source[local.name()] == b"local")
        );
        for source in [
            "Dim value()\nOption Explicit",
            "Public value(1)\nAttribute VB_Name = \"Late\"",
            "Dim value():",
            "Dim value(1):Private other(2)",
            "Const Seed(1)=1",
            "Sub Run(value(1))\nEnd Sub",
            "Sub Run(Optional value()=1)\nEnd Sub",
        ] {
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn module_array_variables_reject_every_incomplete_or_richer_dimension_tail() {
        for bounds in [
            "(",
            "(1",
            "(1 To",
            "(1 To)",
            "(To 1)",
            "(1 To 2 To 3)",
            "(,)",
            "(,1)",
            "(1,)",
            "(1,,2)",
            "(1, ,2)",
            "(1 2)",
            "(())",
            "((1))",
            "(1)(2)",
            "(value)",
            "([value])",
            "(True)",
            "(False)",
            "(Nothing)",
            "(Rem)",
            "(1 To value)",
            "(value To 1)",
            "(1ToFoo)",
            "(1+2)",
            "(1-2)",
            "(1*2)",
            "(1/2)",
            "(1^2)",
            "(1 & 2)",
            "(1.2)",
            "(.2)",
            "(1e2)",
            "(&H10)",
            "(&O10)",
            "(1%)",
            "(1&)",
            "(1!)",
            "(1#)",
            "(1@)",
            "(1$)",
            "(\"1\")",
            "(#1#)",
            "(+)",
            "(-)",
            "(--1)",
            "(+-1)",
            "(-+1)",
            "(+ +1)",
            "(1 To --2)",
            "(1 To 2,)",
            "(1 To 2, [upper])",
            "(1\n)",
            "(1\r\n)",
            "(1\x0c)",
            "(1 ' tail\n)",
            "(1 Rem tail)",
            "(1:=2)",
            "(é)",
        ] {
            let source = format!(
                "Dim prior()\nConst Seed=1\nSub Accepted()\nEnd Sub\nPrivate value{bounds}\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{bounds:?}"
            );
        }
        for tail in [
            "=1",
            ",other(upper)",
            " As",
            " As Product",
            " As Long()",
            " As Long$",
            " extra",
            "$",
            "!",
        ] {
            let source = format!("Dim prior(1)\nDim value(1){tail}\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{tail}"
            );
        }
    }

    #[test]
    fn module_array_variables_keep_existing_source_statement_and_retention_limits() {
        let source = b"Dim first()\nPublic second(1,2) As Variant\nPrivate third(-1 To +1)\n";
        let limits = ModuleLimits::new(source.len(), 3, 0)
            .with_max_parameters(0)
            .with_max_arguments(0);
        let inventory = module_inventory(source, limits, &mut || Ok::<(), ()>(())).unwrap();
        assert_eq!(inventory.statement_count(), 3);
        assert_eq!(inventory.variables().len(), 3);
        assert!(inventory.procedures().is_empty());
        for maximum in [0, 1, 2] {
            assert_eq!(
                module_inventory(
                    source,
                    ModuleLimits::new(source.len(), maximum, 0),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::StatementLimit)
            );
        }
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len() - 1, 3, 0),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::SourceLimit)
        );
        let source = b"Dim first(1)\nConst Seed=1\nSub Run()\nDim local As Long\nlocal=Seed\nEnd Sub\nPrivate last()\n";
        let inventory =
            module_inventory(source, ModuleLimits::new(source.len(), 7, 1), &mut || {
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_eq!(inventory.statement_count(), 7);
        assert_eq!(inventory.variables().len(), 2);
        assert_eq!(inventory.constants().len(), 1);
        assert_eq!(inventory.procedures()[0].statements().len(), 2);
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 6, 1), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::StatementLimit)
        );
        let mut variables = inventory.variables().to_vec();
        let preserved = variables.clone();
        assert_eq!(
            reserve_variables_exact(&mut variables, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert_eq!(variables, preserved);
    }

    #[test]
    fn module_array_variables_cancel_at_every_scan_and_retention_checkpoint() {
        let source = format!(
            "Dim first()\nPrivate _\r\n [wide]( - _\n {} To _\r\n +{}, _\n 3) _\r\n As Long\nConst Seed=1\nSub Run()\nDim local As Long\nEnd Sub\n",
            "9".repeat(1024),
            "8".repeat(1024)
        );
        let limits = ModuleLimits::new(source.len(), 6, 1);
        let mut successful_checks = 0;
        let inventory = module_inventory(source.as_bytes(), limits, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(inventory.variables().len(), 2);
        assert!(successful_checks > 20);
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source.as_bytes(), limits, &mut || {
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
        let bounds = format!("( -{} To +{}, 3)", "9".repeat(1024), "8".repeat(1024));
        let mut successful_checks = 0;
        let mut cursor = 0;
        let scanned = numeric_array_bounds(bounds.as_bytes(), &mut cursor, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap()
        .unwrap();
        assert_eq!(scanned.span, 0..bounds.len());
        assert_eq!(scanned.work_nodes, 8);
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            let mut cursor = 0;
            assert_eq!(
                numeric_array_bounds(bounds.as_bytes(), &mut cursor, &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err("stop"),
                "bounds checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
    }

    #[test]
    fn module_array_variable_all_byte_boundaries_require_full_numeric_groups() {
        for byte in 0..=u8::MAX {
            for (prefix, suffix, accepted) in [
                (
                    b"(".as_slice(),
                    b"2)".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'+' | b'-') || byte.is_ascii_digit(),
                ),
                (
                    b"(1".as_slice(),
                    b")".as_slice(),
                    matches!(byte, b' ' | b'\t') || byte.is_ascii_digit(),
                ),
                (
                    b"(".as_slice(),
                    b")".as_slice(),
                    matches!(byte, b' ' | b'\t') || byte.is_ascii_digit(),
                ),
                (b"(1 ".as_slice(), b" 2)".as_slice(), byte == b','),
                (
                    b"(1 To ".as_slice(),
                    b"2)".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'+' | b'-') || byte.is_ascii_digit(),
                ),
                (
                    b"(1,".as_slice(),
                    b"2)".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'+' | b'-') || byte.is_ascii_digit(),
                ),
                (
                    b"(1".as_slice(),
                    b"To2)".as_slice(),
                    matches!(byte, b' ' | b'\t') || byte.is_ascii_digit(),
                ),
                (
                    b"(1 T".as_slice(),
                    b"2)".as_slice(),
                    matches!(byte, b'o' | b'O'),
                ),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let mut cursor = 0;
                let result =
                    numeric_array_bounds(&source, &mut cursor, &mut || Ok::<(), ()>(())).unwrap();
                assert_eq!(
                    result
                        .as_ref()
                        .is_some_and(|bounds| bounds.span.end == source.len()),
                    accepted,
                    "array byte {byte}, prefix {prefix:?}"
                );
                if let Some(bounds) = result {
                    assert_eq!(bounds.span, 0..cursor);
                    assert!(cursor <= source.len());
                    assert!(bounds.span.len() >= 2);
                    assert_eq!(source[bounds.span.start], b'(');
                    assert_eq!(source[bounds.span.end - 1], b')');
                    assert!(bounds.work_nodes <= bounds.span.len());
                }
            }
            let source = [b"Dim value(".as_slice(), &[byte], b"2)".as_slice()].concat();
            let result = parse(&source);
            assert_eq!(
                result.is_ok(),
                matches!(byte, b' ' | b'\t' | b'+' | b'-') || byte.is_ascii_digit(),
                "module array byte {byte}"
            );
            if let Ok(inventory) = result {
                let variable = &inventory.variables()[0];
                assert_eq!(variable.array_bounds().unwrap(), 9..source.len());
                assert_eq!(variable.declarator().end, source.len());
                assert!(variable.type_range().is_none());
            }
            let source = [b"Dim value(1)".as_slice(), &[byte], b"As Long".as_slice()].concat();
            let result = parse(&source);
            assert_eq!(
                result.is_ok(),
                matches!(byte, b' ' | b'\t' | b'\''),
                "array type boundary byte {byte}"
            );
            if let Ok(inventory) = result {
                let variable = &inventory.variables()[0];
                assert_eq!(&source[variable.array_bounds().unwrap()], b"(1)");
                if byte == b'\'' {
                    assert_eq!(&source[variable.declarator()], b"value(1)");
                    assert!(variable.type_range().is_none());
                } else {
                    assert_eq!(&source[variable.type_range().unwrap()], b"Long");
                }
            }
        }
    }

    #[test]
    fn named_optional_defaults_preserve_exact_atoms_and_source_absence() {
        for newline in ["\n", "\r\n"] {
            let source = format!(
                "Const Seed As Long=7{newline}Sub Run(Optional ByVal first As Long=Seed,Optional [second] _{newline} = _{newline} [True],Optional third=TrueRem,Optional fourth=True,Optional absent){newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(inventory.constants().len(), 1);
            let parameters = inventory.procedures()[0].parameters();
            assert_eq!(parameters.len(), 5);
            for (parameter, spelling, kind) in [
                (&parameters[0], "Seed", ModuleScalarValueKind::Name),
                (&parameters[1], "[True]", ModuleScalarValueKind::Name),
                (&parameters[2], "TrueRem", ModuleScalarValueKind::Name),
                (&parameters[3], "True", ModuleScalarValueKind::Boolean),
            ] {
                assert!(parameter.is_optional());
                let value = parameter.default_value().unwrap();
                assert_eq!(&source[value.span()], spelling);
                assert_eq!(value.kind(), kind);
                assert_eq!(parameter.declaration().end, value.span().end);
            }
            assert_eq!(
                parameters[0].passing_mode(),
                Some(ModuleParameterPassingMode::ByVal)
            );
            assert_eq!(&source[parameters[0].type_range().unwrap()], "Long");
            for parameter in &parameters[1..] {
                assert!(parameter.type_range().is_none());
            }
            assert_eq!(
                &source[parameters[1].declaration()],
                format!("Optional [second] _{newline} = _{newline} [True]")
            );
            assert!(parameters[4].default_value().is_none());
        }
        for spelling in [
            "name",
            "seed",
            "TrueFalse",
            "FalseRem",
            "Remember",
            "Rem1",
            "[Me]",
            "[Rem]",
            "[Nothing]",
            "_seed",
        ] {
            let source = format!("Sub Run(Optional value={spelling})\nEnd Sub\n");
            let inventory = parse(source.as_bytes()).unwrap();
            let value = inventory.procedures()[0].parameters()[0]
                .default_value()
                .unwrap();
            assert_eq!(value.kind(), ModuleScalarValueKind::Name, "{spelling}");
            assert_eq!(&source[value.span()], spelling);
        }
    }

    #[test]
    fn named_defaults_do_not_expand_const_initializers_or_accept_partial_tails() {
        for statement in [
            "Const Limit As Long=Seed",
            "Const Limit=Seed",
            "Public Const Limit=[True]",
        ] {
            assert!(
                matches!(
                    parse(statement.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{statement}"
            );
        }
        for statement in ["Const Limit As Long=Seed", "Const Limit As Long=[True]"] {
            let source = format!("Sub Run(Optional value=Seed)\n{statement}\nEnd Sub\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{statement}"
            );
        }
        for clause in [
            "value=Seed",
            "ByVal value As Long=[True]",
            "Optional value=",
            "Optional value=Seed.member",
            "Optional value=Seed()",
            "Optional value=[Seed].member",
            "Optional value=Seed+1",
            "Optional value=Seed tail",
            "Optional value=Seed$",
            "Optional value=[Seed]$",
            "Optional value=Seeds(1)",
            "Optional value=Null",
            "Optional value=Nothing",
            "Optional value=Empty",
            "Optional value=Me",
            "Optional value=Rem",
            "Optional value=1Seed",
            "Optional value=[Seed",
            "Optional value=[]",
            "Optional value=[Seed name]",
            "Optional value=é",
            "Optional value=Seed,,Optional other=1",
            "Optional value=Seed,",
        ] {
            let source = format!(
                "Const Seed=7\nSub Accepted(Optional first=Seed)\nEnd Sub\nSub Rejected(Optional prior=Seed,{clause})\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{clause}"
            );
        }
    }

    #[test]
    fn named_optional_defaults_keep_complete_lists_under_the_parameter_quota() {
        let clauses: Vec<_> = (0..64)
            .map(|index| format!("Optional value{index}=Seed{index}"))
            .collect();
        let source = format!(
            "Sub First({})\nEnd Sub\nSub Second(Optional [last]=[Seed])\nEnd Sub\n",
            clauses.join(",")
        );
        let limits = ModuleLimits::new(source.len(), 4, 2).with_max_parameters(65);
        let inventory =
            module_inventory(source.as_bytes(), limits, &mut || Ok::<(), ()>(())).unwrap();
        assert_eq!(inventory.procedures()[0].parameters().len(), 64);
        assert_eq!(inventory.procedures()[1].parameters().len(), 1);
        for (index, parameter) in inventory.procedures()[0].parameters().iter().enumerate() {
            let value = parameter.default_value().unwrap();
            assert_eq!(value.kind(), ModuleScalarValueKind::Name);
            assert_eq!(&source[value.span()], format!("Seed{index}"));
        }
        for maximum in [0, 1, 63, 64] {
            assert_eq!(
                module_inventory(
                    source.as_bytes(),
                    limits.with_max_parameters(maximum),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::ParameterLimit)
            );
        }
    }

    #[test]
    fn module_constants_preserve_literal_type_visibility_and_all_source_ranges() {
        for newline in ["\n", "\r\n"] {
            let first = format!(
                "pUbLiC _{newline} cOnSt _{newline} [Limit] _{newline} As _{newline} Long _{newline} = _{newline} 007"
            );
            let second = "Private Const Message=\"Rem: ' = End Sub, \"\"quoted\"\"\"";
            let source = format!(
                "Attribute VB_Name = \"Constants\"{newline}Option Explicit{newline}  {first} ' tail{newline}{second}{newline}Const Enabled As Boolean=tRuE{newline}Const Absent=False{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            assert!(inventory.procedures().is_empty());
            assert_eq!(inventory.constants().len(), 4);
            let constants = inventory.constants();
            assert_eq!(&source[constants[0].statement()], first);
            assert_eq!(&source[constants[0].name()], "[Limit]");
            assert_eq!(
                &source[constants[0].declarator()],
                format!("[Limit] _{newline} As _{newline} Long _{newline} = _{newline} 007")
            );
            assert_eq!(&source[constants[0].type_range().unwrap()], "Long");
            assert_eq!(
                constants[0].visibility(),
                Some(ModuleConstantVisibility::Public)
            );
            assert_eq!(&source[constants[0].visibility_range().unwrap()], "pUbLiC");
            assert_eq!(&source[constants[0].value().span()], "007");
            assert_eq!(
                constants[0].value().kind(),
                ModuleScalarValueKind::UnsignedDecimal
            );
            assert_eq!(&source[constants[1].statement()], second);
            assert_eq!(
                constants[1].visibility(),
                Some(ModuleConstantVisibility::Private)
            );
            assert_eq!(&source[constants[1].visibility_range().unwrap()], "Private");
            assert!(constants[1].type_range().is_none());
            assert_eq!(constants[1].value().kind(), ModuleScalarValueKind::String);
            assert_eq!(
                &source[constants[1].value().span()],
                "\"Rem: ' = End Sub, \"\"quoted\"\"\""
            );
            assert_eq!(&source[constants[2].type_range().unwrap()], "Boolean");
            assert_eq!(constants[2].value().kind(), ModuleScalarValueKind::Boolean);
            assert_eq!(&source[constants[2].value().span()], "tRuE");
            assert!(constants[3].type_range().is_none());
            for constant in &constants[2..] {
                assert_eq!(constant.visibility(), None);
                assert_eq!(constant.visibility_range(), None);
            }
            for constant in constants {
                assert_eq!(constant.declarator().start, constant.name().start);
                assert_eq!(constant.declarator().end, constant.value().span().end);
                assert!(constant.statement().start <= constant.declarator().start);
                assert!(constant.declarator().end <= constant.statement().end);
            }
        }
        for ty in [
            "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
            "Double", "Currency", "Date", "Variant", "Object",
        ] {
            let source = format!("Const Value As {ty}=1");
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(&source[inventory.constants()[0].type_range().unwrap()], ty);
        }
    }

    #[test]
    fn module_constants_can_surround_procedures_but_close_the_leading_header() {
        let source = b"Const Before=1\nSub First()\nConst Local As Long=2\nEnd Sub\nPrivate Const Between As Variant=3\nFunction Second(Optional value=Before) As Long\nSecond=value\nEnd Function\nPublic Const After=4\nConst Before=5\n";
        let inventory = parse(source).unwrap();
        assert_eq!(inventory.constants().len(), 4);
        assert_eq!(inventory.procedures().len(), 2);
        let first = &inventory.procedures()[0];
        assert_eq!(first.statements().len(), 1);
        assert!(
            matches!(&first.statements()[0], ModuleBodyStatement::Declaration(local) if &source[local.name()] == b"Local")
        );
        assert_eq!(&source[inventory.constants()[0].name()], b"Before");
        assert_eq!(&source[inventory.constants()[3].name()], b"Before");
        assert!(inventory.constants()[0].statement().end < first.header().start);
        assert!(first.terminator().end < inventory.constants()[1].statement().start);
        assert!(
            inventory.procedures()[1].terminator().end < inventory.constants()[2].statement().start
        );
        for tail in [
            "Option Explicit",
            "Option Base 0",
            "Attribute VB_Name = \"Late\"",
        ] {
            let source = format!("Const Before=1\n{tail}\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{tail}"
            );
        }
        for source in [
            "Const Value=1:",
            "Const Value=1:Const Other=2",
            ":Const Value=1",
            "Const Value=1\n:",
            "Sub First()\nEnd Sub:Const Value=1",
        ] {
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn malformed_module_constants_never_publish_preceding_structural_records() {
        for statement in [
            "Const",
            "Const Value",
            "Const Value=",
            "Const Value As",
            "Const Value As=1",
            "Const Value As Product=1",
            "Const Value As [Long]=1",
            "Const Value As Long()=1",
            "Const Value As New Object=1",
            "Const Value As String * 8=\"a\"",
            "Const Value As Long$=1",
            "Const Value$=1",
            "Const [Value]$=1",
            "Const Value()=1",
            "Const holder.Value=1",
            "Const Value=1,Other=2",
            "Const Value=1,",
            "Const Value=Seed",
            "Const Value=[True]",
            "Const Value=Null",
            "Const Value=Nothing",
            "Const Value=Empty",
            "Const Value=-1",
            "Const Value=+1",
            "Const Value=1.0",
            "Const Value=.1",
            "Const Value=1e2",
            "Const Value=&H1",
            "Const Value=#1/1/2000#",
            "Const Value=1%",
            "Const Value=TrueRem",
            "Const Value=1+2",
            "Const Value=(1)",
            "Const Value=Make()",
            "Const Value=1 extra",
            "Const Value:=1",
            "Const Value==1",
            "Const Value=\"a\" \"b\"",
            "Const Value=\"open",
            "Const Value=\"continued _\n ",
            "Const []=1",
            "Const [Value name]=1",
            "Const é=1",
            "Const Value=1 Rem tail",
            "Public Private Const Value=1",
            "Private Public Const Value=1",
            "Public Static Const Value=1",
            "Friend Const Value=1",
            "Static Const Value=1",
            "PublicConst Value=1",
            "ConstValue=1",
            "Const [Value]As Long=1",
        ] {
            let source =
                format!("Const Seed=7\nSub Accepted(Optional first=Seed)\nEnd Sub\n{statement}\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{statement:?}"
            );
        }
    }

    #[test]
    fn module_constants_share_exact_statement_quota_without_procedure_cost() {
        let source = b"Const First=1\nPrivate Const Second=\"two\"\nPublic Const Third=True\n";
        let limits = ModuleLimits::new(source.len(), 3, 0)
            .with_max_parameters(0)
            .with_max_arguments(0);
        let inventory = module_inventory(source, limits, &mut || Ok::<(), ()>(())).unwrap();
        assert_eq!(inventory.constants().len(), 3);
        assert_eq!(inventory.statement_count(), 3);
        assert!(inventory.procedures().is_empty());
        for maximum in [0, 1, 2] {
            assert_eq!(
                module_inventory(
                    source,
                    ModuleLimits::new(source.len(), maximum, 0),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::StatementLimit)
            );
        }
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len() - 1, 3, 0),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::SourceLimit)
        );
        let source = b"Const First=1\nSub Run()\nConst Local As Long=2\nEnd Sub\nConst Last=3\n";
        let inventory =
            module_inventory(source, ModuleLimits::new(source.len(), 5, 1), &mut || {
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_eq!(inventory.statement_count(), 5);
        assert_eq!(inventory.constants().len(), 2);
        assert_eq!(inventory.procedures()[0].statements().len(), 1);
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 4, 1), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::StatementLimit)
        );
        let mut many = String::new();
        for index in 0..128 {
            many.push_str(&format!("Const Value{index}={index}\n"));
        }
        let inventory = module_inventory(
            many.as_bytes(),
            ModuleLimits::new(many.len(), 128, 0),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(inventory.constants().len(), 128);
        for (index, constant) in inventory.constants().iter().enumerate() {
            assert_eq!(&many[constant.name()], format!("Value{index}"));
        }
    }

    #[test]
    fn module_constant_retention_preserves_allocation_and_cancellation_errors() {
        let source = b"Private Const [Value]=1";
        let constant = module_constant(source, 0..source.len(), &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap();
        let mut constants = Vec::new();
        assert_eq!(
            reserve_constants(&mut constants, 0, &mut || Ok::<(), ()>(())),
            Err(ModuleError::StatementLimit)
        );
        reserve_constants(&mut constants, 1, &mut || Ok::<(), ()>(())).unwrap();
        constants.push(constant.clone());
        assert_eq!(
            reserve_constants(&mut constants, 1, &mut || Ok::<(), ()>(())),
            Err(ModuleError::StatementLimit)
        );
        assert_eq!(
            reserve_constants_exact(&mut constants, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert_eq!(constants, vec![constant]);
        for cancelled_at in [1, 2] {
            let mut empty = Vec::new();
            let mut calls = 0;
            assert_eq!(
                reserve_constants_exact(&mut empty, 1, &mut || {
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
        }
    }

    #[test]
    fn module_constant_and_named_default_scans_cancel_at_every_checkpoint() {
        let source = format!(
            "Const First=1\nPrivate Const [{}] _\r\n As String=\"{}\"\nSub Run(Optional value={})\nEnd Sub\nConst Last=2\n",
            "x".repeat(1024),
            "a\"\"".repeat(256),
            "y".repeat(1024)
        );
        let limits = ModuleLimits::new(source.len(), 5, 1);
        let mut successful_checks = 0;
        let inventory = module_inventory(source.as_bytes(), limits, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(inventory.constants().len(), 3);
        assert_eq!(inventory.procedures()[0].parameters().len(), 1);
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source.as_bytes(), limits, &mut || {
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
    fn module_constant_and_named_default_all_byte_boundaries_require_complete_tokens() {
        for byte in 0..=u8::MAX {
            for (prefix, suffix, accepted) in [
                (
                    b"Const [Value]".as_slice(),
                    b"As Long=1".as_slice(),
                    matches!(byte, b' ' | b'\t'),
                ),
                (
                    b"Const [Value]=".as_slice(),
                    b"1".as_slice(),
                    byte.is_ascii_digit() || matches!(byte, b' ' | b'\t'),
                ),
                (
                    b"Const [Value]=1".as_slice(),
                    b"".as_slice(),
                    byte.is_ascii_digit()
                        || byte.is_ascii_whitespace() && byte != b'\r'
                        || byte == b'\'',
                ),
                (
                    b"Const [Value]=True".as_slice(),
                    b"".as_slice(),
                    byte.is_ascii_whitespace() && byte != b'\r' || byte == b'\'',
                ),
                (
                    b"Sub Run(Optional value=Seed".as_slice(),
                    b")\nEnd Sub".as_slice(),
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b' ' | b'\t' | b'\x0c'),
                ),
                (
                    b"Sub Run(Optional value=[Seed]".as_slice(),
                    b")\nEnd Sub".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'\x0c'),
                ),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let result = parse(&source);
                assert_eq!(
                    result.is_ok(),
                    accepted,
                    "boundary byte {byte}, prefix {prefix:?}"
                );
                if let Ok(inventory) = result {
                    for constant in inventory.constants() {
                        assert!(constant.statement().start < constant.statement().end);
                        assert!(constant.statement().end <= source.len());
                        assert_eq!(constant.declarator().end, constant.value().span().end);
                    }
                }
            }
        }
    }

    #[test]
    fn complete_modules_preserve_exact_procedure_ranges_and_trivia() {
        let source = b"Attribute VB_Name = \"Main_Module\"\r\nOption Explicit\r\nOption Base 1\r\nOption Compare Text\r\nOption Private Module\r\n' heading\r\n  Public Static Sub [Run]() ' header\r\n\r\n  Rem ignored End Function\r\n  End Sub ' finish\r\nPrivate Function Value$(): ' body\r\nEnd Function";
        let inventory = parse(source).unwrap();
        assert_eq!(inventory.source_len(), source.len());
        assert_eq!(&source[inventory.module_name().unwrap()], b"Main_Module");
        assert_eq!(inventory.procedures().len(), 2);
        let first = &inventory.procedures()[0];
        assert_eq!(first.kind(), ProcedureKind::Sub);
        assert_eq!(&source[first.header()], b"Public Static Sub [Run]()");
        assert_eq!(&source[first.name()], b"[Run]");
        assert_eq!(
            &source[first.body()],
            b"\r\n  Rem ignored End Function\r\n  "
        );
        assert_eq!(&source[first.terminator()], b"End Sub");
        assert!(source[first.declaration()].starts_with(b"Public Static Sub"));
        assert!(source[first.declaration()].ends_with(b"End Sub"));
        let second = &inventory.procedures()[1];
        assert_eq!(second.kind(), ProcedureKind::Function);
        assert_eq!(&source[second.name()], b"Value$");
        assert_eq!(&source[second.body()], b" ' body\r\n");
        assert_eq!(&source[second.terminator()], b"End Function");
    }

    #[test]
    fn accepted_headers_options_and_line_continuations_are_complete() {
        for header in [
            "Sub Run",
            "Private Sub Run()",
            "Static Sub [Run]()",
            "Public _\r\n Static Sub Run( _\n )",
            "Function Value()",
            "Function Value%()",
            "Function [Value]() As Long",
            "Private Static Function Value( _\n ) _\r\n As _\n Long",
        ] {
            let ending = if header.contains("Function") {
                "Function"
            } else {
                "Sub"
            };
            let source = format!("{header}\nEnd _\r\n{ending}\n");
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(inventory.procedures().len(), 1, "{header}");
            assert_eq!(&source[inventory.procedures()[0].header()], header);
            assert!(inventory.procedures()[0].parameters().is_empty());
        }
        for option in [
            "Option Explicit",
            "Option Base 0",
            "Option Base 1",
            "Option Compare Binary",
            "Option Compare Text",
            "Option Compare Database",
            "Option Private Module",
            "option _\n compare _\r\n text",
        ] {
            assert!(parse(option.as_bytes()).is_ok(), "{option}");
        }
        assert!(
            parse(b"' comment\nRem comment\n\n")
                .unwrap()
                .procedures()
                .is_empty()
        );
        assert!(parse(b"").unwrap().procedures().is_empty());
    }

    #[test]
    fn scalar_parameter_lists_preserve_source_ranges_modifiers_and_continuations() {
        for newline in ["\n", "\r\n"] {
            let first = format!("bYvAl _{newline} [first] _{newline} As _{newline} Long");
            let second = "ByRef second As String";
            let third = "last As Boolean";
            let header = format!(
                "Public Static Sub Run( _{newline} {first} _{newline} , _{newline} {second}, {third} _{newline} )"
            );
            let source = format!(
                "Attribute VB_Name = \"ParameterModule\"{newline}  {header} ' header{newline}  Let [first]=1: Call Finish(){newline}End Sub{newline}Function Value%(ByRef [input] As Integer){newline}End Function{newline}Sub Finish(){newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            let procedure = &inventory.procedures()[0];
            assert_eq!(&source[procedure.header()], header);
            assert_eq!(procedure.parameters().len(), 3);
            for (parameter, declaration, name, ty, passing_mode) in [
                (
                    &procedure.parameters()[0],
                    first.as_str(),
                    "[first]",
                    "Long",
                    Some(ModuleParameterPassingMode::ByVal),
                ),
                (
                    &procedure.parameters()[1],
                    second,
                    "second",
                    "String",
                    Some(ModuleParameterPassingMode::ByRef),
                ),
                (&procedure.parameters()[2], third, "last", "Boolean", None),
            ] {
                assert_eq!(&source[parameter.declaration()], declaration);
                assert_eq!(&source[parameter.name()], name);
                assert_eq!(&source[parameter.type_range().unwrap()], ty);
                assert_eq!(parameter.passing_mode(), passing_mode);
                assert!(procedure.header().start < parameter.declaration().start);
                assert!(parameter.declaration().end < procedure.header().end);
                assert!(parameter.declaration().start <= parameter.name().start);
                assert!(parameter.name().end < parameter.type_range().unwrap().start);
                assert_eq!(
                    parameter.type_range().unwrap().end,
                    parameter.declaration().end
                );
            }
            assert_eq!(procedure.statements().len(), 2);
            let function = &inventory.procedures()[1];
            assert_eq!(&source[function.name()], "Value%");
            assert_eq!(
                &source[function.parameters()[0].declaration()],
                "ByRef [input] As Integer"
            );
            assert_eq!(&source[function.parameters()[0].name()], "[input]");
            assert!(inventory.procedures()[2].parameters().is_empty());
        }
    }

    #[test]
    fn required_parameters_admit_each_intrinsic_type_and_written_passing_mode() {
        for ty in [
            "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
            "Double", "Currency", "Date", "Variant", "Object",
        ] {
            for (modifier, mode) in [
                ("", None),
                ("ByVal ", Some(ModuleParameterPassingMode::ByVal)),
                ("bYrEf\t", Some(ModuleParameterPassingMode::ByRef)),
            ] {
                for (category, return_clause) in [("Sub", ""), ("Function", " As Double")] {
                    let declaration = format!("{modifier}value As {ty}");
                    let header = format!("{category} Run({declaration}){return_clause}");
                    let source = format!("{header}\nEnd {category}\n");
                    let inventory = parse(source.as_bytes()).unwrap();
                    let procedure = &inventory.procedures()[0];
                    assert_eq!(&source[procedure.header()], header);
                    assert_eq!(procedure.parameters().len(), 1);
                    let parameter = &procedure.parameters()[0];
                    assert_eq!(&source[parameter.declaration()], declaration);
                    assert_eq!(&source[parameter.name()], "value");
                    assert_eq!(&source[parameter.type_range().unwrap()], ty);
                    assert_eq!(parameter.passing_mode(), mode);
                    assert!(!parameter.is_optional());
                    assert!(parameter.default_value().is_none());
                }
            }
        }
        for source in [
            "Sub Run([ByVal] As Long,[Optional] As String)\nEnd Sub\n",
            "Function Result(ByRef [ParamArray] As Long) As String\nEnd Function\n",
        ] {
            assert!(parse(source.as_bytes()).is_ok(), "{source}");
        }
    }

    #[test]
    fn unsupported_parameter_lists_and_header_tails_reject_prior_inventory() {
        for parameters in [
            "value trailing",
            "value As",
            "value As Long extra",
            "value As Long=1",
            "value As Long:=1",
            "value As Long,",
            ",value As Long",
            "value As Long,,other As Long",
            "value As Long,other As",
            "value As Long other As Long",
            "value() As Long",
            "value(1) As Long",
            "value As Long()",
            "Optional value As Long =",
            "ParamArray value() As Variant",
            "ByVal ByRef value As Long",
            "ByVal ByVal value As Long",
            "ByRef ByRef value As Long",
            "ByRef ByVal value As Long",
            "ByVal[value] As Long",
            "ByVal As Long",
            "ByRef As Long",
            "As As",
            "Optional As Long",
            "ParamArray As Long",
            "value$ As String",
            "value! As Single",
            "[value]$ As String",
            "value As Product",
            "value As [Long]",
            "value As Types.Result",
            "value As New Object",
            "value As String * 8",
            "value As Long$",
            "[value As Long",
            "[] As Long",
            "[value name] As Long",
            "[value$] As String",
            "1value As Long",
            "\"value\" As Long",
            "value As Long\n",
            "value As Long ' inline comment\n",
            "value As Long, _\n",
            "value As Long;other As Long",
        ] {
            let source = format!(
                "Sub Accepted()\nDim previous As Long\nEnd Sub\nSub Rejected({parameters})\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{parameters:?}"
            );
        }
        for header in [
            "Sub Rejected(value As Long) As Long",
            "Sub Rejected(value As Long) trailing",
            "Sub Rejected(value As Long)()",
            "Sub Rejected(value As Long",
            "Sub Rejected value As Long",
            "Function Rejected(value As Long)As Long",
            "Function Rejected$(value As Long) As String",
            "Function Rejected(value As Long) As Product",
            "Function Rejected(value As Long) As Long extra",
            "Function Rejected(value As Long) As Long()",
        ] {
            let category = if header.starts_with("Function") {
                "Function"
            } else {
                "Sub"
            };
            let source = format!("Sub Accepted()\nEnd Sub\n{header}\nEnd {category}\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{header}"
            );
        }
    }

    #[test]
    fn parameter_retention_is_aggregate_and_independent_of_statement_count() {
        let source = b"Sub First(a As Long,ByVal b As String)\nEnd Sub\nFunction Second(ByRef c As Boolean,d As Byte,e As Date) As Long\nEnd Function\n";
        let expected = parse(source).unwrap();
        assert_eq!(expected.statement_count(), 4);
        assert_eq!(expected.procedures()[0].parameters().len(), 2);
        assert_eq!(expected.procedures()[1].parameters().len(), 3);
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len(), 4, 2).with_max_parameters(5),
                &mut || Ok::<(), ()>(())
            ),
            Ok(expected)
        );
        for maximum in [0, 1, 2, 4] {
            assert_eq!(
                module_inventory(
                    source,
                    ModuleLimits::new(source.len(), 4, 2).with_max_parameters(maximum),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::ParameterLimit),
                "{maximum}"
            );
        }
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 4, 2), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::ParameterLimit)
        );
        for (limits, error) in [
            (
                ModuleLimits::new(source.len() - 1, 4, 2),
                ModuleError::SourceLimit,
            ),
            (
                ModuleLimits::new(source.len(), 3, 2),
                ModuleError::StatementLimit,
            ),
            (
                ModuleLimits::new(source.len(), 4, 1),
                ModuleError::ProcedureLimit,
            ),
        ] {
            assert_eq!(
                module_inventory(source, limits.with_max_parameters(5), &mut || Ok::<(), ()>(
                    ()
                )),
                Err(error)
            );
        }
        assert!(
            module_inventory(
                b"Sub Empty()\nEnd Sub\n",
                ModuleLimits::new(20, 2, 1).with_max_parameters(0),
                &mut || Ok::<(), ()>(())
            )
            .unwrap()
            .procedures()[0]
                .parameters()
                .is_empty()
        );
        let many = (0..128)
            .map(|index| format!("item{index} As Long"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("Sub Run({many})\nEnd Sub\n");
        let inventory = module_inventory(
            source.as_bytes(),
            ModuleLimits::new(source.len(), 2, 1).with_max_parameters(128),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(inventory.procedures()[0].parameters().len(), 128);
        assert_eq!(inventory.statement_count(), 2);
        assert_eq!(
            module_inventory(
                source.as_bytes(),
                ModuleLimits::new(source.len(), 2, 1).with_max_parameters(127),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::ParameterLimit)
        );
    }

    #[test]
    fn parameter_scans_and_retention_propagate_cancellation_without_inventory() {
        let header = format!(
            "Sub Run(ByVal {} As Long, _\r\n ByRef second{}As String{} )",
            "x".repeat(8192),
            " _\n".repeat(1024),
            " _\r\n".repeat(1024)
        );
        let mut successful_checks = 0;
        let parsed = closed_header(header.as_bytes(), 2, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap()
        .unwrap();
        assert_eq!(parsed.parameters.len(), 2);
        for limit in [1, successful_checks / 2, successful_checks - 1] {
            let mut calls = 0;
            assert!(matches!(
                closed_header(header.as_bytes(), 2, &mut || {
                    calls += 1;
                    if calls == limit { Err("stop") } else { Ok(()) }
                }),
                Err(ModuleError::Cancelled("stop"))
            ));
            assert_eq!(calls, limit);
        }
        let source = format!("{header}\nEnd Sub\n");
        let mut successful_checks = 0;
        module_inventory(
            source.as_bytes(),
            ModuleLimits::new(source.len(), 2, 1).with_max_parameters(2),
            &mut || {
                successful_checks += 1;
                Ok::<(), ()>(())
            },
        )
        .unwrap();
        for limit in [1, successful_checks / 2, successful_checks - 2] {
            let mut calls = 0;
            assert_eq!(
                module_inventory(
                    source.as_bytes(),
                    ModuleLimits::new(source.len(), 2, 1).with_max_parameters(2),
                    &mut || {
                        calls += 1;
                        if calls == limit { Err("stop") } else { Ok(()) }
                    }
                ),
                Err(ModuleError::Cancelled("stop"))
            );
            assert_eq!(calls, limit);
        }
    }

    #[test]
    fn parameter_storage_failures_preserve_entries_and_callback_errors() {
        let mut parameters = Vec::new();
        assert_eq!(
            reserve_parameters(&mut parameters, 0, &mut || Ok::<(), ()>(())),
            Err(ModuleError::ParameterLimit)
        );
        assert_eq!(parameters.capacity(), 0);
        reserve_parameters(&mut parameters, 1, &mut || Ok::<(), ()>(())).unwrap();
        let parameter = scalar_parameter(b"value As Long", &mut 0, &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap();
        parameters.push(parameter.clone());
        assert_eq!(
            reserve_parameters(&mut parameters, 1, &mut || Ok::<(), ()>(())),
            Err(ModuleError::ParameterLimit)
        );
        assert_eq!(
            reserve_parameters_exact(&mut parameters, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert_eq!(parameters, vec![parameter]);
        let mut empty = Vec::new();
        assert_eq!(
            reserve_parameters_exact(&mut empty, 1, &mut || Err("before")),
            Err(ModuleError::Cancelled("before"))
        );
        assert_eq!(empty.capacity(), 0);
        let mut calls = 0;
        assert_eq!(
            reserve_parameters_exact(&mut empty, 1, &mut || {
                calls += 1;
                if calls == 2 { Err("after") } else { Ok(()) }
            }),
            Err(ModuleError::Cancelled("after"))
        );
        assert_eq!(calls, 2);
        assert!(empty.is_empty());
    }

    #[test]
    fn parameter_byte_boundaries_preserve_ranges_or_reject_the_complete_module() {
        for byte in 0..=u8::MAX {
            let source = [
                b"Sub Run(value".as_slice(),
                &[byte],
                b" As Long)\nEnd Sub\n",
            ]
            .concat();
            if let Ok(inventory) = parse(&source) {
                let procedure = &inventory.procedures()[0];
                assert_eq!(procedure.parameters().len(), 1);
                for range in [
                    procedure.parameters()[0].declaration(),
                    procedure.parameters()[0].name(),
                    procedure.parameters()[0].type_range().unwrap(),
                ] {
                    assert!(range.start <= range.end && range.end <= source.len());
                    assert!(procedure.header().start < range.start);
                    assert!(range.end < procedure.header().end);
                }
            }
        }
        for source in [
            b"Sub Run(value As Long\r)\nEnd Sub".as_slice(),
            b"Sub Run(value As Long\0)\nEnd Sub",
            b"Sub Run([value As Long)\nEnd Sub",
            b"Sub Run(value As Long, ' comment\nother As Long)\nEnd Sub",
        ] {
            assert!(matches!(
                parse(source),
                Err(ModuleError::Unsupported { .. })
            ));
        }
    }

    #[test]
    fn optional_parameter_lists_preserve_exact_ranges_default_absence_and_trivia() {
        for newline in ["\n", "\r\n"] {
            let first = "required As Long";
            let second = format!(
                "oPtIoNaL _{newline} bYvAl _{newline} [label] _{newline} As _{newline} String _{newline} = _{newline} \"a,\"\"b\"\":) As Rem\""
            );
            let third = "Optional ByRef count As Long=001";
            let fourth = "Optional [Optional] As Boolean = fAlSe";
            let fifth = "Optional absent As Variant";
            let header = format!(
                "Public Function Run({first}, _{newline} {second} _{newline}, {third}, {fourth}, {fifth} _{newline} ) As Long"
            );
            let source = format!(
                "Attribute VB_Name = \"OptionalModule\"{newline}  {header} ' header{newline}End Function{newline}Sub Plain([Optional] As Long){newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            let procedure = &inventory.procedures()[0];
            assert_eq!(&source[procedure.header()], header);
            assert_eq!(procedure.parameters().len(), 5);
            for (index, declaration, name, ty, mode, optional, default) in [
                (0, first, "required", "Long", None, false, None),
                (
                    1,
                    second.as_str(),
                    "[label]",
                    "String",
                    Some(ModuleParameterPassingMode::ByVal),
                    true,
                    Some((
                        super::super::ModuleScalarValueKind::String,
                        "\"a,\"\"b\"\":) As Rem\"",
                    )),
                ),
                (
                    2,
                    third,
                    "count",
                    "Long",
                    Some(ModuleParameterPassingMode::ByRef),
                    true,
                    Some((super::super::ModuleScalarValueKind::UnsignedDecimal, "001")),
                ),
                (
                    3,
                    fourth,
                    "[Optional]",
                    "Boolean",
                    None,
                    true,
                    Some((super::super::ModuleScalarValueKind::Boolean, "fAlSe")),
                ),
                (4, fifth, "absent", "Variant", None, true, None),
            ] {
                let parameter = &procedure.parameters()[index];
                assert_eq!(&source[parameter.declaration()], declaration);
                assert_eq!(&source[parameter.name()], name);
                assert_eq!(&source[parameter.type_range().unwrap()], ty);
                assert_eq!(parameter.passing_mode(), mode);
                assert_eq!(parameter.is_optional(), optional);
                assert!(procedure.header().start < parameter.declaration().start);
                assert!(parameter.declaration().end < procedure.header().end);
                if let Some((kind, spelling)) = default {
                    let value = parameter.default_value().unwrap();
                    assert_eq!(value.kind(), kind);
                    assert_eq!(&source[value.span()], spelling);
                    assert!(parameter.type_range().unwrap().end < value.span().start);
                    assert_eq!(value.span().end, parameter.declaration().end);
                } else {
                    assert!(parameter.default_value().is_none());
                    assert_eq!(
                        parameter.type_range().unwrap().end,
                        parameter.declaration().end
                    );
                }
            }
            let bracketed = &inventory.procedures()[1].parameters()[0];
            assert!(!bracketed.is_optional());
            assert!(bracketed.default_value().is_none());
            assert_eq!(&source[bracketed.name()], "[Optional]");
        }
        let inventory = parse(b"Sub Run(Optional first As Long=1,required As String,Optional last As Boolean)\nEnd Sub\n").unwrap();
        assert_eq!(inventory.procedures()[0].parameters().len(), 3);
        assert!(!inventory.procedures()[0].parameters()[1].is_optional());
    }

    #[test]
    fn optional_parameter_intrinsic_types_modes_and_literal_kinds_are_source_only() {
        for ty in [
            "String", "Boolean", "Byte", "Integer", "Long", "LongLong", "LongPtr", "Single",
            "Double", "Currency", "Date", "Variant", "Object",
        ] {
            for (modifier, passing) in [
                ("", None),
                ("ByVal ", Some(ModuleParameterPassingMode::ByVal)),
                ("bYrEf\t", Some(ModuleParameterPassingMode::ByRef)),
            ] {
                for (suffix, expected) in [
                    ("", None),
                    (
                        "=00019",
                        Some((
                            super::super::ModuleScalarValueKind::UnsignedDecimal,
                            "00019",
                        )),
                    ),
                    (
                        " = \"a\"\"b\"",
                        Some((super::super::ModuleScalarValueKind::String, "\"a\"\"b\"")),
                    ),
                    (
                        "=tRuE",
                        Some((super::super::ModuleScalarValueKind::Boolean, "tRuE")),
                    ),
                ] {
                    let declaration = format!("Optional {modifier}value As {ty}{suffix}");
                    let source = format!("Sub Run({declaration})\nEnd Sub\n");
                    let inventory = parse(source.as_bytes()).unwrap();
                    let parameter = &inventory.procedures()[0].parameters()[0];
                    assert!(parameter.is_optional());
                    assert_eq!(parameter.passing_mode(), passing);
                    assert_eq!(&source[parameter.declaration()], declaration);
                    assert_eq!(&source[parameter.type_range().unwrap()], ty);
                    if let Some((kind, spelling)) = expected {
                        let value = parameter.default_value().unwrap();
                        assert_eq!(value.kind(), kind);
                        assert_eq!(&source[value.span()], spelling);
                    } else {
                        assert!(parameter.default_value().is_none());
                    }
                }
            }
        }
        for spelling in ["\"\"", "\"\"\"\"", "True", "False", "0", "000"] {
            let source =
                format!("Function Run$(Optional value As String={spelling})\nEnd Function\n");
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(
                &source[inventory.procedures()[0].parameters()[0]
                    .default_value()
                    .unwrap()
                    .span()],
                spelling
            );
        }
    }

    #[test]
    fn optional_parameter_name_positions_use_complete_tails_after_modifiers() {
        for (passing, mode) in [
            ("", None),
            ("ByVal ", Some(ModuleParameterPassingMode::ByVal)),
            ("ByRef ", Some(ModuleParameterPassingMode::ByRef)),
        ] {
            let names: &[&str] = if passing.is_empty() {
                &["Optional", "As"]
            } else {
                &["Optional", "As", "ByVal", "ByRef"]
            };
            for name in names {
                for default in ["", " = 1"] {
                    let declaration = format!("Optional {passing}{name} As Long{default}");
                    let source = format!("Sub Run({declaration})\nEnd Sub\n");
                    let inventory = parse(source.as_bytes()).unwrap();
                    let parameter = &inventory.procedures()[0].parameters()[0];
                    assert!(parameter.is_optional());
                    assert_eq!(parameter.passing_mode(), mode);
                    assert_eq!(&source[parameter.declaration()], declaration);
                    assert_eq!(&source[parameter.name()], *name);
                    assert_eq!(&source[parameter.type_range().unwrap()], "Long");
                    if default.is_empty() {
                        assert!(parameter.default_value().is_none());
                    } else {
                        assert_eq!(&source[parameter.default_value().unwrap().span()], "1");
                    }
                }
            }
        }
        for clause in [
            "Optional bYvAl oPtIoNaL As Long=001",
            "Optional bYrEf aS As Long",
            "Optional [ParamArray] As Long",
            "Optional [Rem] As Long=1",
        ] {
            assert!(
                parse(format!("Sub Run({clause})\nEnd Sub\n").as_bytes()).is_ok(),
                "{clause}"
            );
        }
        for clause in [
            "Optional Optional value As Long",
            "Optional ByVal ByVal value As Long",
            "Optional ByRef ByRef value As Long",
            "Optional As As",
            "Optional As As Long extra",
            "Optional ByVal Optional As Long = name+1",
            "Optional ParamArray As Long",
            "Optional ByVal ParamArray As Long",
            "Optional Rem As Long",
            "Optional ByRef Rem As Long",
        ] {
            let source = format!(
                "Sub Accepted(Optional previous As Long=1)\nEnd Sub\nSub Rejected(Optional complete As Long=1,{clause})\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{clause}"
            );
        }
    }

    #[test]
    fn required_parameter_keyword_names_follow_the_authored_name_position() {
        for (passing, mode) in [
            ("", None),
            ("ByVal ", Some(ModuleParameterPassingMode::ByVal)),
            ("ByRef ", Some(ModuleParameterPassingMode::ByRef)),
        ] {
            let names: &[&str] = if passing.is_empty() {
                &["As"]
            } else {
                &["Optional", "As", "ByVal", "ByRef"]
            };
            for name in names {
                let declaration = format!("{passing}{name} As Long");
                let source = format!("Sub Run({declaration})\nEnd Sub\n");
                let inventory = parse(source.as_bytes()).unwrap();
                let parameter = &inventory.procedures()[0].parameters()[0];
                assert_eq!(&source[parameter.declaration()], declaration);
                assert_eq!(&source[parameter.name()], *name);
                assert_eq!(&source[parameter.type_range().unwrap()], "Long");
                assert_eq!(parameter.passing_mode(), mode);
                assert!(!parameter.is_optional());
                assert!(parameter.default_value().is_none());
            }
        }
        for clause in [
            "bYvAl oPtIoNaL As Long",
            "bYrEf bYvAl As String",
            "bYvAl bYrEf As Boolean",
            "aS As Long",
            "[Optional] As Long",
        ] {
            let source = format!("Sub Run({clause})\nEnd Sub\n");
            let inventory = parse(source.as_bytes()).unwrap();
            let parameter = &inventory.procedures()[0].parameters()[0];
            assert!(!parameter.is_optional());
            assert!(parameter.default_value().is_none());
            assert_eq!(&source[parameter.declaration()], clause);
        }
        for clause in [
            "Optional As Long",
            "ByVal As Long",
            "ByRef As Long",
            "As As",
            "ByVal ByVal value As Long",
            "ByRef ByRef value As Long",
            "ByVal Optional value As Long",
            "ByRef As As Long trailing",
            "ByVal Optional As Long=1",
            "ByRef ByVal As Long=1",
            "Rem As Long",
            "ByVal Rem As Long",
            "ParamArray As Long",
            "ByRef ParamArray As Long",
        ] {
            let source = format!(
                "Sub Accepted()\nEnd Sub\nSub Rejected(complete As Long,{clause})\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{clause}"
            );
        }
    }

    #[test]
    fn optional_parameter_default_and_modifier_tails_reject_entire_inventory() {
        for parameters in [
            "Optional value As Long =",
            "Optional value As Long = name.member",
            "Optional value As Long = [True].member",
            "Optional value As Long = Null",
            "Optional value As Long = Nothing",
            "Optional value As Long = Empty",
            "Optional value As Long = -1",
            "Optional value As Long = +1",
            "Optional value As Long = 1.0",
            "Optional value As Long = .1",
            "Optional value As Long = 1e2",
            "Optional value As Long = &H1",
            "Optional value As Long = &O1",
            "Optional value As Long = #1/1/2000#",
            "Optional value As Long = 1%",
            "Optional value As Long = 1+2",
            "Optional value As Long = True And False",
            "Optional value As Long = (1)",
            "Optional value As Long = Make()",
            "Optional value As Long = holder.value",
            "Optional value As Long = 1 extra",
            "Optional value As Long = TrueFalse()",
            "Optional value As Long = True$",
            "Optional value As Long := 1",
            "Optional value As Long ==1",
            "Optional value As String = \"a\" \"b\"",
            "Optional value As String = \"open",
            "Optional value As String = \"continued _\n ",
            "Optional value As String = \"line\nend\"",
            "Optional value As Long = 1 ' inside\n",
            "Optional value As Long = 1,",
            "Optional value As Long = 1,,other As Long",
            "Optional value As Long = 1,Optional next As Long =",
            "Optional Optional value As Long",
            "Optional ByVal ByRef value As Long",
            "Optional ByRef ByVal value As Long",
            "Optional ByVal ByVal value As Long",
            "ByVal Optional value As Long",
            "ByRef Optional value As Long",
            "Optional[value] As Long",
            "Optional ByVal[value] As Long",
            "Optional As Long",
            "Optional ByVal As Long",
            "Optional ParamArray value() As Variant",
            "Optional value() As Long",
            "Optional value As Long()",
            "Optional value As Product",
            "Optional value As [Long]",
            "Optional value As New Object",
            "Optional value As String * 8",
            "Optional value As Long$",
            "Optional value$ As String",
            "Optional value = seed.member",
            "Optional value = 1 + 2",
            "Optional value As",
            "required As Long=1",
        ] {
            let source = format!(
                "Sub Accepted(Optional previous As Long=1)\nDim local As Long\nEnd Sub\nSub Rejected(required As Long,Optional complete As String=\"kept\",{parameters})\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{parameters:?}"
            );
        }
        for header in [
            "Sub Rejected(Optional value As Long=1) tail",
            "Sub Rejected(Optional value As Long=1)()",
            "Sub Rejected(Optional value As Long=1",
            "Sub Rejected(Optional value As Long=1) As Long",
            "Function Rejected(Optional value As Long=1) As Product",
            "Function Rejected(Optional value As Long=1) As Long extra",
        ] {
            let category = if header.starts_with("Function") {
                "Function"
            } else {
                "Sub"
            };
            let source = format!("Sub Accepted()\nEnd Sub\n{header}\nEnd {category}\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{header}"
            );
        }
    }

    #[test]
    fn optional_parameter_retention_remains_aggregate_and_bounded() {
        let source = b"Sub First(Optional a As Long=1,ByVal b As String)\nEnd Sub\nFunction Second(Optional ByRef c As Boolean=False,Optional d As Variant,e As Date) As Long\nEnd Function\n";
        let limits = ModuleLimits::new(source.len(), 4, 2).with_max_parameters(5);
        let expected = parse(source).unwrap();
        assert_eq!(expected.statement_count(), 4);
        assert_eq!(
            module_inventory(source, limits, &mut || Ok::<(), ()>(())),
            Ok(expected)
        );
        for maximum in [0, 1, 2, 4] {
            assert_eq!(
                module_inventory(source, limits.with_max_parameters(maximum), &mut || Ok::<
                    (),
                    (),
                >(
                    ()
                )),
                Err(ModuleError::ParameterLimit)
            );
        }
        for (limits, expected) in [
            (
                ModuleLimits::new(source.len() - 1, 4, 2),
                ModuleError::SourceLimit,
            ),
            (
                ModuleLimits::new(source.len(), 3, 2),
                ModuleError::StatementLimit,
            ),
            (
                ModuleLimits::new(source.len(), 4, 1),
                ModuleError::ProcedureLimit,
            ),
        ] {
            assert_eq!(
                module_inventory(source, limits.with_max_parameters(5), &mut || Ok::<(), ()>(
                    ()
                )),
                Err(expected)
            );
        }
        let list = (0..256_usize)
            .map(|index| {
                if index.is_multiple_of(2) {
                    format!("Optional value{index} As Long={index}")
                } else {
                    format!("Optional value{index} As Variant")
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("Sub Run({list})\nEnd Sub\n");
        for maximum in [256, usize::MAX] {
            let inventory = module_inventory(
                source.as_bytes(),
                ModuleLimits::new(source.len(), 2, 1).with_max_parameters(maximum),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap();
            let parameters = inventory.procedures()[0].parameters();
            assert_eq!(parameters.len(), 256);
            for (index, parameter) in parameters.iter().enumerate() {
                assert!(parameter.is_optional());
                assert_eq!(&source[parameter.name()], format!("value{index}"));
                if index.is_multiple_of(2) {
                    assert_eq!(
                        &source[parameter.default_value().unwrap().span()],
                        index.to_string()
                    );
                } else {
                    assert!(parameter.default_value().is_none());
                }
            }
        }
        assert_eq!(
            module_inventory(
                source.as_bytes(),
                ModuleLimits::new(source.len(), 2, 1).with_max_parameters(255),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::ParameterLimit)
        );
    }

    #[test]
    fn optional_parameter_byte_boundaries_are_complete_not_prefixes() {
        for byte in 0..=u8::MAX {
            for (prefix, suffix, accepted) in [
                (
                    b"Sub Run(Optional value As Long=1".as_slice(),
                    b")\nEnd Sub\n".as_slice(),
                    byte.is_ascii_digit() || matches!(byte, b' ' | b'\t' | b'\x0c'),
                ),
                (
                    b"Sub Run(Optional value As Long=".as_slice(),
                    b"1)\nEnd Sub\n".as_slice(),
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b' ' | b'\t' | b'\x0c'),
                ),
                (
                    b"Sub Run(Optional value As Boolean=True".as_slice(),
                    b")\nEnd Sub\n".as_slice(),
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b' ' | b'\t' | b'\x0c'),
                ),
                (
                    b"Sub Run(Optional value As String=\"a\"".as_slice(),
                    b")\nEnd Sub\n".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'\x0c'),
                ),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let result = parse(&source);
                assert_eq!(
                    result.is_ok(),
                    accepted,
                    "boundary byte {byte}, prefix {prefix:?}"
                );
                if let Ok(inventory) = result {
                    let procedure = &inventory.procedures()[0];
                    let parameter = &procedure.parameters()[0];
                    assert!(parameter.is_optional());
                    let value = parameter.default_value().unwrap();
                    assert!(parameter.type_range().unwrap().end < value.span().start);
                    assert!(value.span().end < procedure.header().end);
                    assert_eq!(value.span().end, parameter.declaration().end);
                }
            }
        }
    }

    #[test]
    fn optional_parameter_every_checkpoint_cancellation_never_returns_inventory() {
        let header = format!(
            "Sub Run(Optional ByVal [{}] As Long={} _\r\n,Optional ByRef text As String=\"{}\",Optional absent As Variant)",
            "x".repeat(1024),
            "9".repeat(1024),
            "a\"\"".repeat(512)
        );
        let mut successful_checks = 0;
        let parsed = closed_header(header.as_bytes(), 3, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap()
        .unwrap();
        assert_eq!(parsed.parameters.len(), 3);
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            assert!(
                matches!(
                    closed_header(header.as_bytes(), 3, &mut || {
                        calls += 1;
                        if calls == cancelled_at {
                            Err("stop")
                        } else {
                            Ok(())
                        }
                    }),
                    Err(ModuleError::Cancelled("stop"))
                ),
                "header checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
        let source = format!(
            "Sub Accepted(Optional previous As Boolean=True)\nEnd Sub\n{header}\nEnd Sub\n"
        );
        let limits = ModuleLimits::new(source.len(), 4, 2).with_max_parameters(4);
        let mut successful_checks = 0;
        let inventory = module_inventory(source.as_bytes(), limits, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(inventory.procedures().len(), 2);
        assert_eq!(inventory.procedures()[1].parameters().len(), 3);
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source.as_bytes(), limits, &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err(ModuleError::Cancelled("stop")),
                "module checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
    }

    #[test]
    fn optional_parameter_inline_defaults_preserve_allocation_error_rules() {
        let text = b"Optional ByVal value As String=\"a\"\"b\"";
        let parameter = scalar_parameter(text, &mut 0, &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap();
        assert!(parameter.default_value().is_some());
        let mut parameters = Vec::new();
        reserve_parameters(&mut parameters, 1, &mut || Ok::<(), ()>(())).unwrap();
        parameters.push(parameter.clone());
        assert_eq!(
            reserve_parameters(&mut parameters, 1, &mut || Ok::<(), ()>(())),
            Err(ModuleError::ParameterLimit)
        );
        assert_eq!(
            reserve_parameters_exact(&mut parameters, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert_eq!(parameters, vec![parameter]);
        for cancelled_at in [1, 2] {
            let mut empty = Vec::new();
            let mut calls = 0;
            assert_eq!(
                reserve_parameters_exact(&mut empty, 1, &mut || {
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
        }
    }

    #[test]
    fn untyped_parameter_mixed_lists_preserve_authored_ranges_and_type_absence() {
        for newline in ["\n", "\r\n"] {
            let first = format!("bYvAl _{newline} [first]");
            let second = "written As Variant";
            let third = format!(
                "Optional _{newline} ByRef _{newline} [label] _{newline} = _{newline} \"a,\"\"b\"\":) As Rem\""
            );
            let fourth = "Optional absent";
            let fifth = "Optional count=001";
            let sixth = "Optional flag As Boolean=fAlSe";
            let header = format!(
                "Public Function Run( _{newline} {first} _{newline} , {second}, _{newline} {third}, {fourth}, {fifth}, {sixth} _{newline} ) As Long"
            );
            let source = format!(
                "Attribute VB_Name = \"MixedParameters\"{newline}' before{newline}  {header} ' header{newline}  Let [first]=1: Call Finish(){newline}End Function{newline}Sub Finish(ByRef input){newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            let procedure = &inventory.procedures()[0];
            assert_eq!(&source[procedure.header()], header);
            assert_eq!(procedure.parameters().len(), 6);
            assert_eq!(procedure.statements().len(), 2);
            for (index, declaration, name, ty, passing, optional, default) in [
                (
                    0,
                    first.as_str(),
                    "[first]",
                    None,
                    Some(ModuleParameterPassingMode::ByVal),
                    false,
                    None,
                ),
                (1, second, "written", Some("Variant"), None, false, None),
                (
                    2,
                    third.as_str(),
                    "[label]",
                    None,
                    Some(ModuleParameterPassingMode::ByRef),
                    true,
                    Some((ModuleScalarValueKind::String, "\"a,\"\"b\"\":) As Rem\"")),
                ),
                (3, fourth, "absent", None, None, true, None),
                (
                    4,
                    fifth,
                    "count",
                    None,
                    None,
                    true,
                    Some((ModuleScalarValueKind::UnsignedDecimal, "001")),
                ),
                (
                    5,
                    sixth,
                    "flag",
                    Some("Boolean"),
                    None,
                    true,
                    Some((ModuleScalarValueKind::Boolean, "fAlSe")),
                ),
            ] {
                let parameter = &procedure.parameters()[index];
                assert_eq!(&source[parameter.declaration()], declaration);
                assert_eq!(&source[parameter.name()], name);
                assert_eq!(parameter.passing_mode(), passing);
                assert_eq!(parameter.is_optional(), optional);
                assert!(procedure.header().start < parameter.declaration().start);
                assert!(parameter.declaration().end < procedure.header().end);
                assert_eq!(parameter.type_range().map(|range| &source[range]), ty);
                let last_token_end = parameter
                    .type_range()
                    .map_or(parameter.name().end, |range| range.end);
                if let Some((kind, spelling)) = default {
                    let value = parameter.default_value().unwrap();
                    assert_eq!(value.kind(), kind);
                    assert_eq!(&source[value.span()], spelling);
                    assert!(last_token_end < value.span().start);
                    assert_eq!(value.span().end, parameter.declaration().end);
                } else {
                    assert!(parameter.default_value().is_none());
                    assert_eq!(last_token_end, parameter.declaration().end);
                }
            }
            let parameter = &inventory.procedures()[1].parameters()[0];
            assert_eq!(&source[parameter.declaration()], "ByRef input");
            assert!(parameter.type_range().is_none());
        }
    }

    #[test]
    fn untyped_parameter_modes_literals_and_bracketed_names_retain_only_source_syntax() {
        let source = b"Sub Run(value)\nEnd Sub";
        let inventory = parse(source).unwrap();
        let parameters = inventory.procedures()[0].parameters();
        assert_eq!(parameters.len(), 1);
        assert_eq!(&source[parameters[0].name()], b"value");
        assert!(parameters[0].type_range().is_none());
        assert!(!parameters[0].is_optional());
        assert!(parameters[0].default_value().is_none());
        for (modifier, passing) in [
            ("", None),
            ("ByVal ", Some(ModuleParameterPassingMode::ByVal)),
            ("bYrEf\t", Some(ModuleParameterPassingMode::ByRef)),
        ] {
            for name in [
                "value",
                "_value",
                "[Optional]",
                "[ByVal]",
                "[ByRef]",
                "[As]",
                "[ParamArray]",
                "[Rem]",
            ] {
                for (category, return_clause) in [("Sub", ""), ("Function", " As String")] {
                    let declaration = format!("{modifier}{name}");
                    let source =
                        format!("{category} Run({declaration}){return_clause}\nEnd {category}\n");
                    let inventory = parse(source.as_bytes()).unwrap();
                    let parameter = &inventory.procedures()[0].parameters()[0];
                    assert_eq!(&source[parameter.declaration()], declaration);
                    assert_eq!(&source[parameter.name()], name);
                    assert_eq!(parameter.passing_mode(), passing);
                    assert!(!parameter.is_optional());
                    assert!(parameter.type_range().is_none());
                    assert!(parameter.default_value().is_none());
                }
                for (tail, expected) in [
                    ("", None),
                    (
                        " = 00019",
                        Some((ModuleScalarValueKind::UnsignedDecimal, "00019")),
                    ),
                    ("=\"\"", Some((ModuleScalarValueKind::String, "\"\""))),
                    (
                        "=\"a\"\"b\"",
                        Some((ModuleScalarValueKind::String, "\"a\"\"b\"")),
                    ),
                    ("=tRuE", Some((ModuleScalarValueKind::Boolean, "tRuE"))),
                    ("=False", Some((ModuleScalarValueKind::Boolean, "False"))),
                ] {
                    let declaration = format!("Optional {modifier}{name}{tail}");
                    let source = format!("Sub Run({declaration})\nEnd Sub\n");
                    let inventory = parse(source.as_bytes()).unwrap();
                    let parameter = &inventory.procedures()[0].parameters()[0];
                    assert_eq!(&source[parameter.declaration()], declaration);
                    assert_eq!(&source[parameter.name()], name);
                    assert_eq!(parameter.passing_mode(), passing);
                    assert!(parameter.is_optional());
                    assert!(parameter.type_range().is_none());
                    if let Some((kind, spelling)) = expected {
                        let value = parameter.default_value().unwrap();
                        assert_eq!(value.kind(), kind);
                        assert_eq!(&source[value.span()], spelling);
                    } else {
                        assert!(parameter.default_value().is_none());
                    }
                }
            }
        }
        let source = b"Sub Run(absent,written As Variant,Optional omitted,Optional explicit As Variant)\nEnd Sub\n";
        let inventory = parse(source).unwrap();
        let parameters = inventory.procedures()[0].parameters();
        assert_eq!(parameters.len(), 4);
        assert!(parameters[0].type_range().is_none());
        assert_eq!(&source[parameters[1].type_range().unwrap()], b"Variant");
        assert!(parameters[2].type_range().is_none());
        assert_eq!(&source[parameters[3].type_range().unwrap()], b"Variant");
    }

    #[test]
    fn untyped_parameter_invalid_written_types_and_late_tails_reject_entire_inventory() {
        for clause in [
            "value As",
            "value As=1",
            "value As Long extra",
            "value As Product",
            "value As [Long]",
            "value As Types.Result",
            "value As New Object",
            "value As String * 8",
            "value As Long$",
            "[value]As Long",
            "Optional value As",
            "Optional value As=1",
            "Optional value As = 1",
            "Optional value As Product=1",
            "Optional value As Long()=1",
            "value$",
            "value!",
            "[value]$",
            "value()",
            "value(1)",
            "ParamArray value()",
            "Optional ParamArray value()",
            "value extra",
            "ByVal ByRef value",
            "Optional ByVal ByRef value",
            "value=1",
            "ByVal value=1",
            "value,",
            "value,,other",
            ",value",
            "Optional value=",
            "Optional value=seed.member",
            "Optional value=[True].member",
            "Optional value=Null",
            "Optional value=Nothing",
            "Optional value=Empty",
            "Optional value=-1",
            "Optional value=+1",
            "Optional value=1.0",
            "Optional value=.1",
            "Optional value=1e2",
            "Optional value=&H1",
            "Optional value=#1/1/2000#",
            "Optional value=1%",
            "Optional value=1+2",
            "Optional value=True And False",
            "Optional value=(1)",
            "Optional value=Make()",
            "Optional value=holder.value",
            "Optional value=1 extra",
            "Optional value=TrueFalse()",
            "Optional value=True$",
            "Optional value:=1",
            "Optional value==1",
            "Optional value=\"a\" \"b\"",
            "Optional value=\"open",
            "Optional value=\"continued _\n ",
            "Optional value=1,",
            "Optional value=1,,other",
            "Optional value=1,Optional next=",
            "Optional value=1 ' inside\n",
            "value\r",
            "[value",
            "[]",
            "[value name]",
        ] {
            let source = format!(
                "Sub Accepted(previous,Optional complete=1)\nDim local As Long\nEnd Sub\nSub Rejected(first As Variant,second,Optional third=\"kept\",{clause})\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{clause:?}"
            );
        }
        for header in [
            "Sub Rejected(value) As Long",
            "Sub Rejected(value) trailing",
            "Sub Rejected(value)()",
            "Sub Rejected(value",
            "Sub Rejected value",
            "Function Rejected(value)As Long",
            "Function Rejected(value) As",
            "Function Rejected(value) As Product",
            "Function Rejected$(value) As String",
        ] {
            let category = if header.starts_with("Function") {
                "Function"
            } else {
                "Sub"
            };
            let source = format!("Sub Accepted(previous)\nEnd Sub\n{header}\nEnd {category}\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{header}"
            );
        }
    }

    #[test]
    fn untyped_parameter_aggregate_quotas_bound_mixed_lists_across_procedures() {
        let source = b"Sub First(a,ByVal b As String)\nEnd Sub\nFunction Second(Optional ByRef c=False,Optional d As Variant,e) As Long\nEnd Function\n";
        let limits = ModuleLimits::new(source.len(), 4, 2).with_max_parameters(5);
        let expected = parse(source).unwrap();
        assert_eq!(expected.statement_count(), 4);
        assert_eq!(
            module_inventory(source, limits, &mut || Ok::<(), ()>(())),
            Ok(expected)
        );
        for maximum in [0, 1, 2, 4] {
            assert_eq!(
                module_inventory(source, limits.with_max_parameters(maximum), &mut || Ok::<
                    (),
                    (),
                >(
                    ()
                )),
                Err(ModuleError::ParameterLimit)
            );
        }
        for (limits, expected) in [
            (
                ModuleLimits::new(source.len() - 1, 4, 2),
                ModuleError::SourceLimit,
            ),
            (
                ModuleLimits::new(source.len(), 3, 2),
                ModuleError::StatementLimit,
            ),
            (
                ModuleLimits::new(source.len(), 4, 1),
                ModuleError::ProcedureLimit,
            ),
        ] {
            assert_eq!(
                module_inventory(source, limits.with_max_parameters(5), &mut || Ok::<(), ()>(
                    ()
                )),
                Err(expected)
            );
        }
        let empty = b"Sub Empty()\nEnd Sub\n";
        assert!(
            module_inventory(
                empty,
                ModuleLimits::new(empty.len(), 2, 1).with_max_parameters(0),
                &mut || Ok::<(), ()>(())
            )
            .unwrap()
            .procedures()[0]
                .parameters()
                .is_empty()
        );
        let list = (0..256_usize)
            .map(|index| match index % 4 {
                0 => format!("value{index}"),
                1 => format!("ByVal value{index} As Variant"),
                2 => format!("Optional ByRef value{index}={index}"),
                _ => format!("Optional value{index} As Long"),
            })
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("Sub Run({list})\nEnd Sub\n");
        for maximum in [256, usize::MAX] {
            let inventory = module_inventory(
                source.as_bytes(),
                ModuleLimits::new(source.len(), 2, 1).with_max_parameters(maximum),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap();
            let parameters = inventory.procedures()[0].parameters();
            assert_eq!(parameters.len(), 256);
            assert_eq!(inventory.statement_count(), 2);
            for (index, parameter) in parameters.iter().enumerate() {
                assert_eq!(&source[parameter.name()], format!("value{index}"));
                assert_eq!(parameter.type_range().is_some(), matches!(index % 4, 1 | 3));
                assert_eq!(parameter.is_optional(), index % 4 >= 2);
                assert_eq!(parameter.default_value().is_some(), index % 4 == 2);
            }
        }
        assert_eq!(
            module_inventory(
                source.as_bytes(),
                ModuleLimits::new(source.len(), 2, 1).with_max_parameters(255),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::ParameterLimit)
        );
    }

    #[test]
    fn untyped_parameter_all_byte_boundaries_require_complete_authored_tokens() {
        for byte in 0..=u8::MAX {
            for (prefix, suffix, accepted) in [
                (
                    b"Sub Run([value]".as_slice(),
                    b")\nEnd Sub\n".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'\x0c'),
                ),
                (
                    b"Sub Run([value]".as_slice(),
                    b"As Long)\nEnd Sub\n".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'\x0c'),
                ),
                (
                    b"Sub Run(Optional [value]=1".as_slice(),
                    b")\nEnd Sub\n".as_slice(),
                    byte.is_ascii_digit() || matches!(byte, b' ' | b'\t' | b'\x0c'),
                ),
                (
                    b"Sub Run(Optional [value]=".as_slice(),
                    b"1)\nEnd Sub\n".as_slice(),
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b' ' | b'\t' | b'\x0c'),
                ),
                (
                    b"Sub Run(Optional [value]=True".as_slice(),
                    b")\nEnd Sub\n".as_slice(),
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b' ' | b'\t' | b'\x0c'),
                ),
                (
                    b"Sub Run(Optional [value]=\"a\"".as_slice(),
                    b")\nEnd Sub\n".as_slice(),
                    matches!(byte, b' ' | b'\t' | b'\x0c'),
                ),
            ] {
                let source = [prefix, &[byte], suffix].concat();
                let result = parse(&source);
                assert_eq!(
                    result.is_ok(),
                    accepted,
                    "boundary byte {byte}, prefix {prefix:?}"
                );
                if let Ok(inventory) = result {
                    let procedure = &inventory.procedures()[0];
                    assert_eq!(procedure.parameters().len(), 1);
                    let parameter = &procedure.parameters()[0];
                    for range in [parameter.declaration(), parameter.name()] {
                        assert!(range.start <= range.end && range.end <= source.len());
                        assert!(procedure.header().start < range.start);
                        assert!(range.end < procedure.header().end);
                    }
                    if let Some(ty) = parameter.type_range() {
                        assert_eq!(&source[ty], b"Long");
                    } else if let Some(value) = parameter.default_value() {
                        assert!(parameter.name().end < value.span().start);
                        assert_eq!(value.span().end, parameter.declaration().end);
                    } else {
                        assert_eq!(parameter.name().end, parameter.declaration().end);
                    }
                }
            }
        }
    }

    #[test]
    fn untyped_parameter_every_checkpoint_cancellation_returns_no_inventory() {
        let header = format!(
            "Sub Run(ByVal [{}] _\r\n,Optional ByRef text _\n = _\r\n \"{}\",written As Variant,Optional absent{} )",
            "x".repeat(1024),
            "a\"\"".repeat(256),
            " _\r\n".repeat(64)
        );
        let mut successful_checks = 0;
        let parsed = closed_header(header.as_bytes(), 4, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap()
        .unwrap();
        assert_eq!(parsed.parameters.len(), 4);
        assert!(parsed.parameters[0].type_range().is_none());
        assert!(parsed.parameters[1].type_range().is_none());
        assert!(parsed.parameters[2].type_range().is_some());
        assert!(parsed.parameters[3].type_range().is_none());
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            assert!(
                matches!(
                    closed_header(header.as_bytes(), 4, &mut || {
                        calls += 1;
                        if calls == cancelled_at {
                            Err("stop")
                        } else {
                            Ok(())
                        }
                    }),
                    Err(ModuleError::Cancelled("stop"))
                ),
                "header checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
        let source =
            format!("Sub Accepted(previous,Optional initial=True)\nEnd Sub\n{header}\nEnd Sub\n");
        let limits = ModuleLimits::new(source.len(), 4, 2).with_max_parameters(6);
        let mut successful_checks = 0;
        let inventory = module_inventory(source.as_bytes(), limits, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(inventory.procedures().len(), 2);
        assert_eq!(inventory.procedures()[1].parameters().len(), 4);
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source.as_bytes(), limits, &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err(ModuleError::Cancelled("stop")),
                "module checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
    }

    #[test]
    fn untyped_parameter_retention_keeps_allocation_and_cancellation_errors_typed() {
        let parameter = scalar_parameter(b"Optional ByVal [value]=\"a\"\"b\"", &mut 0, &mut || {
            Ok::<(), ()>(())
        })
        .unwrap()
        .unwrap();
        assert!(parameter.type_range().is_none());
        assert!(parameter.default_value().is_some());
        let mut parameters = Vec::new();
        reserve_parameters(&mut parameters, 1, &mut || Ok::<(), ()>(())).unwrap();
        parameters.push(parameter.clone());
        assert_eq!(
            reserve_parameters(&mut parameters, 1, &mut || Ok::<(), ()>(())),
            Err(ModuleError::ParameterLimit)
        );
        assert_eq!(
            reserve_parameters_exact(&mut parameters, usize::MAX, &mut || Ok::<(), ()>(())),
            Err(ModuleError::AllocationFailed)
        );
        assert_eq!(parameters, vec![parameter]);
        for cancelled_at in [1, 2] {
            let mut empty = Vec::new();
            let mut calls = 0;
            assert_eq!(
                reserve_parameters_exact(&mut empty, 1, &mut || {
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
        }
    }

    #[test]
    fn parameter_trivia_comment_only_lists_have_no_formal_or_parameter_cost() {
        for newline in ["\n", "\r\n"] {
            for contents in [
                "Rem",
                "rEm\x0c",
                " \tRem",
                "Rem\x0cRem",
                "\x0cRem\x0c \tRem\x0c",
            ] {
                for (category, return_clause) in [("Sub", ""), ("Function", " As Long")] {
                    let header = format!("{category} Run({contents}){return_clause}");
                    let source = format!("{header} ' header{newline}End {category}{newline}");
                    let inventory = module_inventory(
                        source.as_bytes(),
                        ModuleLimits::new(source.len(), 2, 1).with_max_parameters(0),
                        &mut || Ok::<(), ()>(()),
                    )
                    .unwrap();
                    let procedure = &inventory.procedures()[0];
                    assert_eq!(&source[procedure.header()], header);
                    assert!(procedure.parameters().is_empty());
                    assert_eq!(inventory.statement_count(), 2);
                }
            }
        }
        let source = b"Sub Run(Rem\x0cRem)\nEnd Sub\nSub Actual([Rem])\nEnd Sub\n";
        let inventory = module_inventory(
            source,
            ModuleLimits::new(source.len(), 4, 2).with_max_parameters(1),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert!(inventory.procedures()[0].parameters().is_empty());
        let actual = &inventory.procedures()[1].parameters()[0];
        assert_eq!(&source[actual.name()], b"[Rem]");
        assert!(actual.type_range().is_none());
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len(), 4, 2).with_max_parameters(0),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::ParameterLimit)
        );
    }

    #[test]
    fn parameter_trivia_mixed_lists_preserve_only_actual_declarator_and_token_ranges() {
        for newline in ["\n", "\r\n"] {
            let first = "ByVal\x0c[Rem]";
            let second = "Optional Rem\x0cByRef\x0c[label] Rem\x0cAs Rem\x0cString Rem\x0c= Rem\x0c\"Rem,\"\"As\"\":)\"";
            let third = "[last]";
            let header =
                format!("Sub Run(Rem\x0c{first} Rem, _{newline} Rem\x0c{second} Rem,{third} Rem)");
            let source = format!(
                "Attribute VB_Name = \"TriviaModule\"{newline}  {header} ' header{newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            let procedure = &inventory.procedures()[0];
            assert_eq!(&source[procedure.header()], header);
            assert_eq!(procedure.parameters().len(), 3);
            for (index, declaration, name, ty, mode, optional) in [
                (
                    0,
                    first,
                    "[Rem]",
                    None,
                    Some(ModuleParameterPassingMode::ByVal),
                    false,
                ),
                (
                    1,
                    second,
                    "[label]",
                    Some("String"),
                    Some(ModuleParameterPassingMode::ByRef),
                    true,
                ),
                (2, third, "[last]", None, None, false),
            ] {
                let parameter = &procedure.parameters()[index];
                assert_eq!(&source[parameter.declaration()], declaration);
                assert_eq!(&source[parameter.name()], name);
                assert_eq!(parameter.type_range().map(|range| &source[range]), ty);
                assert_eq!(parameter.passing_mode(), mode);
                assert_eq!(parameter.is_optional(), optional);
            }
            let value = procedure.parameters()[1].default_value().unwrap();
            assert_eq!(value.kind(), ModuleScalarValueKind::String);
            assert_eq!(&source[value.span()], "\"Rem,\"\"As\"\":)\"");
            assert_eq!(
                value.span().end,
                procedure.parameters()[1].declaration().end
            );
            assert_eq!(
                procedure.parameters()[0].declaration().end,
                procedure.parameters()[0].name().end
            );
            assert_eq!(
                procedure.parameters()[2].declaration().end,
                procedure.parameters()[2].name().end
            );
        }
        for name in ["Remnant", "Reminder", "Rem0", "Rem_", "[Rem]"] {
            let source =
                format!("Sub Run({name},ByRef {name} As Variant,Optional {name}=1)\nEnd Sub\n");
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(inventory.procedures()[0].parameters().len(), 3);
            for parameter in inventory.procedures()[0].parameters() {
                assert_eq!(&source[parameter.name()], name);
            }
        }
    }

    #[test]
    fn parameter_trivia_never_supplies_missing_parameters_or_shortens_comment_tails() {
        for clause in [
            "Rem, value",
            "Rem=1",
            "Rem =1",
            "Rem ",
            "Rem\t",
            "Rem As Long",
            "Rem _\r\n ",
            "Rem\r\n",
            "ByVal Rem",
            "ByRef Rem",
            "Optional Rem",
            "Optional ByVal Rem",
            "Optional ByRef Rem",
            "value,Rem",
            "value,Rem\x0c",
            "value,Rem,other",
            "value,Rem\x0c,other",
            "value,\x0c",
            "value,Rem\x0cByVal",
            "Optional Rem=1",
            "Optional Rem\x0c=1",
            "Optional ByVal Rem\x0c=1",
            "Rem\x0c[value] Rem ",
            "value Rem\t",
            "value Rem As Long",
            "[value] Rem\x0cAs Rem\x0c",
            "Optional [value] Rem\x0cAs = 1",
            "Optional [value] Rem\x0c=Rem\x0c",
            "Optional [value]=1 Rem ",
            "Optional [value] Rem\x0c=\"unterminated",
            "Rem\x0c[value] suffix",
            "Rem!",
            "Rem$",
            "Rem\0",
            "Rem\x0b",
            "Rem' comment\n",
        ] {
            let source =
                format!("Sub Accepted(previous)\nEnd Sub\nSub Rejected({clause})\nEnd Sub\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{clause:?}"
            );
        }
    }

    #[test]
    fn parameter_trivia_all_byte_boundaries_distinguish_bare_comments_from_names() {
        for byte in 0..=u8::MAX {
            let source = [b"Sub Run(Rem".as_slice(), &[byte], b")\nEnd Sub\n"].concat();
            let result = parse(&source);
            let is_name = byte.is_ascii_alphanumeric() || byte == b'_';
            assert_eq!(
                result.is_ok(),
                is_name || byte == b'\x0c',
                "Rem boundary {byte}"
            );
            if let Ok(inventory) = result {
                let parameters = inventory.procedures()[0].parameters();
                if is_name {
                    assert_eq!(parameters.len(), 1);
                    assert_eq!(&source[parameters[0].name()], &[b'R', b'e', b'm', byte]);
                    assert!(parameters[0].type_range().is_none());
                } else {
                    assert!(parameters.is_empty());
                }
            }
            let source = [
                b"Sub Run(Rem\x0c[actual]".as_slice(),
                &[byte],
                b")\nEnd Sub\n",
            ]
            .concat();
            let result = parse(&source);
            assert_eq!(
                result.is_ok(),
                matches!(byte, b' ' | b'\t' | b'\x0c'),
                "actual name boundary {byte}"
            );
            if let Ok(inventory) = result {
                let parameters = inventory.procedures()[0].parameters();
                assert_eq!(parameters.len(), 1);
                assert_eq!(&source[parameters[0].name()], b"[actual]");
                assert_eq!(parameters[0].name(), parameters[0].declaration());
            }
        }
    }

    #[test]
    fn parameter_trivia_scans_and_empty_lists_propagate_every_checkpoint_cancellation() {
        let trivia = format!(" {}Rem\x0c", " _\r\nRem\x0c".repeat(64));
        let mut successful_checks = 0;
        assert_eq!(
            parameter_trivia_len(trivia.as_bytes(), &mut || {
                successful_checks += 1;
                Ok::<(), ()>(())
            })
            .unwrap(),
            trivia.len()
        );
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            assert_eq!(
                parameter_trivia_len(trivia.as_bytes(), &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err("stop")
            );
            assert_eq!(calls, cancelled_at);
        }
        for (contents, count) in [
            (trivia.clone(), 0),
            (
                format!("{trivia}ByVal\x0c[value] Rem,{trivia}Optional\x0cother=1 Rem"),
                2,
            ),
        ] {
            let source = format!("Sub Run({contents})\nEnd Sub\n");
            let limits = ModuleLimits::new(source.len(), 2, 1).with_max_parameters(count);
            let mut successful_checks = 0;
            let inventory = module_inventory(source.as_bytes(), limits, &mut || {
                successful_checks += 1;
                Ok::<(), ()>(())
            })
            .unwrap();
            assert_eq!(inventory.procedures()[0].parameters().len(), count);
            for cancelled_at in 1..=successful_checks {
                let mut calls = 0;
                assert_eq!(
                    module_inventory(source.as_bytes(), limits, &mut || {
                        calls += 1;
                        if calls == cancelled_at {
                            Err("stop")
                        } else {
                            Ok(())
                        }
                    }),
                    Err(ModuleError::Cancelled("stop")),
                    "module checkpoint {cancelled_at}, parameter count {count}"
                );
                assert_eq!(calls, cancelled_at);
            }
        }
    }

    #[test]
    fn colon_boundaries_require_a_procedure_header_or_body() {
        for source in [
            ":Sub Run()\nEnd Sub\n",
            " \t:Sub Run()\nEnd Sub\n",
            "Sub Run()\nEnd Sub\n:",
            "Sub Run()\nEnd Sub\n \t: \n",
            "Sub Run()\nEnd Sub\n:Sub Next()\nEnd Sub\n",
            "Option Explicit:Sub Run()\nEnd Sub\n",
            "Attribute VB_Name = \"Main\":Sub Run()\nEnd Sub\n",
            "Sub Run()\nEnd Sub:",
            "Sub Run()\nEnd Sub:Sub Next()\nEnd Sub\n",
        ] {
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{source}"
            );
        }
        for source in [
            "Sub Run(): : End Sub\n",
            "' ordinary comment:",
            "Rem ordinary comment:",
            "Sub Run()\n' body:\nEnd Sub\n' trailing:",
        ] {
            assert!(parse(source.as_bytes()).is_ok(), "{source}");
        }
    }

    #[test]
    fn scalar_local_bodies_preserve_exact_ranges_with_comments_and_colon_boundaries() {
        for newline in ["\n", "\r\n"] {
            let source = format!(
                "Attribute VB_Name = \"BodyModule\"{newline}Sub Run(){newline}  Dim [count] As Long ' local{newline}  Const Limit As Long = 7: Let [count] = Limit{newline}  [count] = \"End Sub: Rem ' = \"\"quoted\"\"\"{newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            let procedure = &inventory.procedures()[0];
            assert_eq!(procedure.statements().len(), 4);
            let ModuleBodyStatement::Declaration(local) = &procedure.statements()[0] else {
                panic!("first body statement is a typed local");
            };
            assert_eq!(&source[local.statement()], "Dim [count] As Long");
            assert_eq!(&source[local.declarator()], "[count] As Long");
            assert_eq!(&source[local.name()], "[count]");
            assert_eq!(&source[local.type_range()], "Long");
            assert!(local.value().is_none());
            let ModuleBodyStatement::Declaration(constant) = &procedure.statements()[1] else {
                panic!("second body statement is a typed constant");
            };
            assert_eq!(&source[constant.declarator()], "Limit As Long = 7");
            assert_eq!(&source[constant.value().unwrap().span()], "7");
            let ModuleBodyStatement::Assignment(assignment) = &procedure.statements()[2] else {
                panic!("third body statement is an assignment");
            };
            assert_eq!(&source[assignment.statement()], "Let [count] = Limit");
            assert_eq!(&source[assignment.destination()], "[count]");
            assert_eq!(&source[assignment.value().span()], "Limit");
            for statement in procedure.statements() {
                assert!(procedure.body().start <= statement.statement().start);
                assert!(statement.statement().end <= procedure.body().end);
            }
            assert_eq!(&source[procedure.terminator()], "End Sub");
            assert!(source[procedure.body()].contains("Const Limit"));
        }
    }

    #[test]
    fn bodies_remain_procedure_owned_and_functions_can_assign_simple_result_names() {
        let source = b"Sub First():Dim value As Long:value=1:End Sub\nFunction Second() As Long\nConst Limit As Long = 2\nLet Second=Limit\nEnd Function\n";
        let inventory = parse(source).unwrap();
        assert_eq!(inventory.procedures().len(), 2);
        assert_eq!(inventory.procedures()[0].statements().len(), 2);
        assert_eq!(inventory.procedures()[1].statements().len(), 2);
        let ModuleBodyStatement::Assignment(assignment) =
            &inventory.procedures()[1].statements()[1]
        else {
            panic!("function result assignment is retained");
        };
        assert_eq!(&source[assignment.destination()], b"Second");
        assert_eq!(&source[assignment.value().span()], b"Limit");
        for procedure in inventory.procedures() {
            for statement in procedure.statements() {
                assert!(statement.statement().start >= procedure.body().start);
                assert!(statement.statement().end <= procedure.body().end);
            }
        }
    }

    #[test]
    fn explicit_calls_preserve_comments_colons_continuations_and_procedure_ownership() {
        for newline in ["\n", "\r\n"] {
            let source = format!(
                "Attribute VB_Name = \"CallModule\"{newline}Sub Run(){newline}  Call Finish ' End Sub: Call ignored(){newline}  Dim value As Long:Call [Finish]( _{newline} ){newline}  value=\"Call ignored(): End Sub\"{newline}  Call _{newline} [Run] _{newline} ( _{newline} ){newline}End Sub{newline}Sub Finish(){newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(inventory.procedures().len(), 2);
            let procedure = &inventory.procedures()[0];
            assert_eq!(procedure.statements().len(), 5);
            let calls: Vec<_> = procedure
                .statements()
                .iter()
                .filter_map(|statement| match statement {
                    ModuleBodyStatement::Call(call) => Some(call),
                    ModuleBodyStatement::Declaration(_)
                    | ModuleBodyStatement::Variables(_)
                    | ModuleBodyStatement::Assignment(_)
                    | ModuleBodyStatement::ApplicationAssignment(_)
                    | ModuleBodyStatement::ExpressionAssignment(_)
                    | ModuleBodyStatement::ExpressionCall(_) => None,
                })
                .collect();
            assert_eq!(calls.len(), 3);
            assert_eq!(&source[calls[0].statement()], "Call Finish");
            assert_eq!(&source[calls[0].callee()], "Finish");
            assert!(calls[0].arguments().is_none());
            assert_eq!(&source[calls[1].callee()], "[Finish]");
            assert_eq!(
                &source[calls[1].arguments().unwrap()],
                format!("( _{newline} )")
            );
            assert_eq!(&source[calls[2].callee()], "[Run]");
            assert_eq!(
                &source[calls[2].statement()],
                format!("Call _{newline} [Run] _{newline} ( _{newline} )")
            );
            for statement in procedure.statements() {
                assert!(procedure.body().start <= statement.statement().start);
                assert!(statement.statement().end <= procedure.body().end);
            }
            for call in calls {
                assert!(call.statement().start < call.callee().start);
                assert!(call.callee().end <= call.statement().end);
                if let Some(arguments) = call.arguments() {
                    assert!(call.callee().end <= arguments.start);
                    assert!(arguments.end <= call.statement().end);
                }
            }
            assert!(inventory.procedures()[1].statements().is_empty());
        }
    }

    #[test]
    fn scalar_call_lists_preserve_module_relative_ranges_and_procedure_ownership() {
        for newline in ["\n", "\r\n"] {
            let parentheses = format!(
                "( _{newline} 007 _{newline}, _{newline} \"a,:='Rem End Sub \"\"quoted\"\"\", [local], FALSE _{newline} )"
            );
            let call_text = format!("Call _{newline} [Finish] _{newline} {parentheses}");
            let source = format!(
                "Attribute VB_Name = \"ArgumentModule\"{newline}Sub Run(){newline}  Dim local As Long{newline}  {call_text} ' header-looking End Sub{newline}  Call Finish:End Sub{newline}Sub Finish(){newline}  Call [Run](){newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(inventory.procedures().len(), 2);
            let procedure = &inventory.procedures()[0];
            assert_eq!(procedure.statements().len(), 3);
            let ModuleBodyStatement::Call(call) = &procedure.statements()[1] else {
                panic!("scalar call retained after the typed local");
            };
            assert_eq!(&source[call.statement()], call_text);
            assert_eq!(&source[call.callee()], "[Finish]");
            assert_eq!(&source[call.arguments().unwrap()], parentheses);
            assert_eq!(call.argument_values().len(), 4);
            for (value, (spelling, kind)) in call.argument_values().iter().zip([
                ("007", ModuleScalarValueKind::UnsignedDecimal),
                (
                    "\"a,:='Rem End Sub \"\"quoted\"\"\"",
                    ModuleScalarValueKind::String,
                ),
                ("[local]", ModuleScalarValueKind::Name),
                ("FALSE", ModuleScalarValueKind::Boolean),
            ]) {
                assert_eq!(&source[value.span()], spelling);
                assert_eq!(value.kind(), kind);
                assert!(procedure.body().start <= value.span().start);
                assert!(value.span().end <= procedure.body().end);
                assert!(call.arguments().unwrap().start < value.span().start);
                assert!(value.span().end < call.arguments().unwrap().end);
            }
            let ModuleBodyStatement::Call(bare) = &procedure.statements()[2] else {
                panic!("bare Call remains retained");
            };
            assert!(bare.argument_values().is_empty());
            assert!(bare.arguments().is_none());
            let ModuleBodyStatement::Call(empty) = &inventory.procedures()[1].statements()[0]
            else {
                panic!("empty Call in the second procedure remains retained");
            };
            assert!(empty.argument_values().is_empty());
            assert_eq!(&source[empty.arguments().unwrap()], "()");
        }
    }

    #[test]
    fn call_argument_quota_is_aggregate_across_lists_and_procedures() {
        let source = b"Sub First():Call Finish(1,True):Call Finish:End Sub\nSub Second()\nCall Finish(value,\"text\",False)\nCall Finish()\nEnd Sub\n";
        let expected = parse(source).unwrap();
        assert_eq!(expected.statement_count(), 8);
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len(), 8, 2).with_max_arguments(5),
                &mut || Ok::<(), ()>(())
            ),
            Ok(expected)
        );
        for maximum in [0, 1, 2, 4] {
            assert_eq!(
                module_inventory(
                    source,
                    ModuleLimits::new(source.len(), 8, 2).with_max_arguments(maximum),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::ArgumentLimit),
                "aggregate quota {maximum}"
            );
        }
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 8, 2), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::ArgumentLimit)
        );
        for (limits, error) in [
            (
                ModuleLimits::new(source.len() - 1, 8, 2),
                ModuleError::SourceLimit,
            ),
            (
                ModuleLimits::new(source.len(), 7, 2),
                ModuleError::StatementLimit,
            ),
            (
                ModuleLimits::new(source.len(), 8, 1),
                ModuleError::ProcedureLimit,
            ),
        ] {
            assert_eq!(
                module_inventory(source, limits.with_max_arguments(5), &mut || Ok::<(), ()>(
                    ()
                )),
                Err(error)
            );
        }
        let zero = b"Sub Run():Call Finish:Call Finish():End Sub\n";
        let inventory = module_inventory(
            zero,
            ModuleLimits::new(zero.len(), 4, 1).with_max_arguments(0),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(inventory.procedures()[0].statements().len(), 2);
        let values = (0..128)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("Sub Run()\nCall Finish({values})\nEnd Sub\n");
        for maximum in [128, usize::MAX] {
            let inventory = module_inventory(
                source.as_bytes(),
                ModuleLimits::new(source.len(), 3, 1).with_max_arguments(maximum),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap();
            let ModuleBodyStatement::Call(call) = &inventory.procedures()[0].statements()[0] else {
                panic!("many scalar arguments retained");
            };
            assert_eq!(call.argument_values().len(), 128);
            for (index, value) in call.argument_values().iter().enumerate() {
                assert_eq!(&source[value.span()], index.to_string());
            }
        }
        assert_eq!(
            module_inventory(
                source.as_bytes(),
                ModuleLimits::new(source.len(), 3, 1).with_max_arguments(127),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::ArgumentLimit)
        );
    }

    #[test]
    fn unsupported_scalar_call_lists_reject_all_preceding_module_records() {
        for body in [
            "Call Finish(1,)",
            "Call Finish(,1)",
            "Call Finish(1,,2)",
            "Call Finish(1,value:=2)",
            "Call Finish(1,ByVal value)",
            "Call Finish(1,target.member)",
            "Call Finish(1,2.0)",
            "Call Finish(1,#1/1/2000#)",
            "Call Finish(1,value$)",
            "Call Finish(1) extra",
            "Call Finish(1)()",
            "Call Finish(1).member",
            "Call Finish 1,2",
            "Call Finish value",
            "Call Name(1)",
            "Call Finish(1,\"open",
            "Call Finish(1,\"continued _\n",
            "Call Finish(1, ' inline\n2)",
            "Call Finish(1\n)",
            "Call Finish(1,\0)",
            "Call Finish(1,\r2)",
        ] {
            let source = format!(
                "Sub Accepted()\nDim previous As Long\nCall Finish(1)\nEnd Sub\nSub Rejected()\nCall Finish(True)\n{body}\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{body:?}"
            );
        }
    }

    #[test]
    fn scalar_call_module_cancellation_never_publishes_a_partial_inventory() {
        let source = b"Sub First(value As Long)\nDim local As Long\nCall Finish(1,local,\"a\"\"b\",False)\nEnd Sub\nSub Second()\nCall Finish(2,True)\nEnd Sub\n";
        let limits = ModuleLimits::new(source.len(), 7, 2)
            .with_max_parameters(1)
            .with_max_arguments(6);
        let mut successful_checks = 0;
        let inventory = module_inventory(source, limits, &mut || {
            successful_checks += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(inventory.procedures().len(), 2);
        for cancelled_at in 1..=successful_checks {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source, limits, &mut || {
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
    fn unsupported_call_statement_rejects_the_whole_module_without_prior_records() {
        for body in [
            "Call",
            "Call()",
            "Call[Finish]",
            "Call\x0cFinish()",
            "Call\x0bFinish()",
            "Call Finish(value:=1)",
            "Call Finish(,)",
            "Call Finish(())",
            "Call Finish()()",
            "Call Finish() trailing",
            "Call Finish = 1",
            "Call Finish other",
            "Call Finish$()",
            "Call Me",
            "Call Nothing()",
            "Call Rem",
            "Call Name",
            "Call Name()",
            "cAlL nAmE",
            "cAlL nAmE()",
            "Call _\n Name()",
            "Call holder.Finish()",
            "Call .Finish()",
            "Call [open",
            "Call Finish(",
            "Call Finish(\n)",
            "Call Finish _\n",
            "Call Finish ' continued _\nCall Finish",
            "Finish()",
            "End Function",
        ] {
            let source = format!("Sub Run()\nCall Finish()\n{body}\nEnd Sub\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{body}"
            );
        }
        for source in [
            "Call Finish",
            "Call Finish()\nSub Run()\nEnd Sub",
            "Sub Run()\nEnd Sub\nCall Finish()",
            "Sub Run()\nCall Finish():End Sub:Sub Finish()\nEnd Sub",
            "Sub Run()\nCall Finish\0\nEnd Sub",
            "Sub Run()\nCall Finish\rEnd Sub",
        ] {
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{source:?}"
            );
        }
    }

    #[test]
    fn calls_share_total_body_limits_and_preserve_resource_error_categories() {
        let source = b"Sub First():Dim value As Long:Call Second():value=1:End Sub\nSub Second()\nCall [First]\nEnd Sub\n";
        let expected = parse(source).unwrap();
        assert_eq!(expected.statement_count(), 8);
        assert_eq!(expected.procedures()[0].statements().len(), 3);
        assert_eq!(expected.procedures()[1].statements().len(), 1);
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 8, 2), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Ok(expected)
        );
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 7, 2), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::StatementLimit)
        );
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len() - 1, 8, 2),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::SourceLimit)
        );
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 8, 1), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::ProcedureLimit)
        );
    }

    #[test]
    fn module_call_scans_propagate_initial_and_late_cancellation_without_inventory() {
        let source = format!(
            "Sub Run()\nCall Finish({})\nEnd Sub\n",
            " _\r\n".repeat(8192)
        );
        let mut successful_checks = 0;
        let expected = module_inventory(
            source.as_bytes(),
            ModuleLimits::new(source.len(), 3, 1),
            &mut || {
                successful_checks += 1;
                Ok::<(), ()>(())
            },
        )
        .unwrap();
        assert_eq!(expected.procedures()[0].statements().len(), 1);
        for limit in [1, successful_checks - 2] {
            let mut calls = 0;
            assert_eq!(
                module_inventory(
                    source.as_bytes(),
                    ModuleLimits::new(source.len(), 3, 1),
                    &mut || {
                        calls += 1;
                        if calls == limit { Err("stop") } else { Ok(()) }
                    }
                ),
                Err(ModuleError::Cancelled("stop"))
            );
            assert_eq!(calls, limit);
        }
    }

    #[test]
    fn contextual_names_stay_admitted_outside_bare_assignment_start() {
        for body in [
            "[Load]=1",
            "[Unload]=1",
            "Let Load=1",
            "lEt UnLoAd=1",
            "Name=1",
            "Line=1",
            "Dim Load As Long",
            "Const Load As Long=1",
            "value=Load",
            "Call Load()",
        ] {
            let source = format!("Sub Run()\n{body}\nEnd Sub\n");
            let inventory = parse(source.as_bytes()).unwrap();
            let statements = inventory.procedures()[0].statements();
            assert_eq!(statements.len(), 1, "{body}");
            assert_eq!(&source[statements[0].statement()], body);
        }
    }

    #[test]
    fn bare_statement_keyword_assignment_heads_reject_prior_supported_records() {
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
                let source =
                    format!("Sub Run()\nDim accepted As Long\naccepted=1\n{spelling}=1\nEnd Sub\n");
                assert!(
                    matches!(
                        parse(source.as_bytes()),
                        Err(ModuleError::Unsupported { .. })
                    ),
                    "{spelling}"
                );
            }
        }
    }

    #[test]
    fn scalar_body_unsupported_statement_rejects_the_whole_module_without_prefixes() {
        for body in [
            "Dim value As Long\nvalue = 1 +",
            "Const Limit As Long = other",
            "Dim item As New Object",
            "Dim item As Product",
            "Dim value As Long=1",
            "value=Me",
            "value=Nothing",
            "value=Null",
            "value=Empty",
            "Load=1",
            "Unload=1",
            "lOaD = 1",
            "uNlOaD = 1",
            "Load _\n = 1",
            "Unload _\r\n = 1",
            "value=True False",
            "value=\"open",
            "value=\"continued _\n",
            "value=target.member",
            "item.member=1",
            "value(0)=1",
            "value$=1",
            "If True Then",
            "#If Enabled Then",
            "Sub Nested()",
            "End Function",
            "Option Explicit",
            "Attribute VB_Name = \"Late\"",
            "Rem-x\nvalue=1",
            "Rem: value=1",
            "value=1 ' continued _\nvalue=2",
        ] {
            let source = format!("Sub Run()\nDim accepted As Long\n{body}\nEnd Sub\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{body}"
            );
        }
        for source in [
            "Dim value As Product\nSub Run()\nEnd Sub",
            "Const Limit As Long=other",
            "value=1",
        ] {
            assert!(matches!(
                parse(source.as_bytes()),
                Err(ModuleError::Unsupported { .. })
            ));
        }
    }

    #[test]
    fn local_variable_lists_preserve_procedure_ownership_comments_colons_and_module_separation() {
        for newline in ["\n", "\r\n"] {
            let local_text =
                format!("Dim _{newline} first, [grid](-1 To +2,3) _{newline} As Long, pending()");
            let source = format!(
                "Dim field, storage(2){newline}Const Seed=7{newline}Sub First(){newline}  {local_text} ' End Sub: ignored{newline}Dim typed As Long:first=Seed:Call Target(first):End Sub{newline}Sub Second(){newline}Dim first, [first] As Variant{newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(inventory.variables().len(), 2);
            assert_eq!(inventory.constants().len(), 1);
            assert_eq!(inventory.procedures().len(), 2);
            let first = &inventory.procedures()[0];
            let ModuleBodyStatement::Variables(list) = &first.statements()[0] else {
                panic!("complete local declarators retained in first procedure");
            };
            assert_eq!(&source[list.statement()], local_text);
            assert_eq!(list.variables().len(), 3);
            assert!(list.variables()[0].type_range().is_none());
            assert_eq!(&source[list.variables()[1].type_range().unwrap()], "Long");
            assert_eq!(
                &source[list.variables()[1].array_bounds().unwrap()],
                "(-1 To +2,3)"
            );
            assert_eq!(list.variables()[1].array_work_nodes(), 8);
            assert_eq!(list.variables()[2].array_work_nodes(), 1);
            for variable in list.variables() {
                assert!(first.body().start <= variable.declarator().start);
                assert!(variable.declarator().end <= first.body().end);
                assert!(
                    inventory
                        .variables()
                        .iter()
                        .all(|module| module.name() != variable.name())
                );
            }
            assert!(
                matches!(&first.statements()[1], ModuleBodyStatement::Declaration(local) if &source[local.name()] == "typed")
            );
            assert!(matches!(
                first.statements()[2],
                ModuleBodyStatement::Assignment(_)
            ));
            assert!(matches!(
                first.statements()[3],
                ModuleBodyStatement::Call(_)
            ));
            let second = &inventory.procedures()[1];
            let ModuleBodyStatement::Variables(list) = &second.statements()[0] else {
                panic!("duplicate local spellings remain independent source records");
            };
            assert_eq!(&source[list.variables()[0].name()], "first");
            assert_eq!(&source[list.variables()[1].name()], "[first]");
            assert_ne!(list.variables()[0].name(), list.variables()[1].name());
            assert!(second.body().start <= list.variables()[0].declarator().start);
            assert!(list.variables()[1].declarator().end <= second.body().end);
        }
        for source in [
            "Dim field\nSub Run()\nDim untyped\nEnd Sub",
            "Dim field,other\nSub Run()\nDim first As Long,second As Long\nEnd Sub",
            "Dim value\nSub Run()\nDim local(1) As Long\nEnd Sub",
            "Sub Run()\nDim x, y As Long\nDim values() As Long\nEnd Sub",
        ] {
            let inventory = parse(source.as_bytes()).unwrap();
            assert!(
                inventory.procedures()[0]
                    .statements()
                    .iter()
                    .all(|statement| matches!(statement, ModuleBodyStatement::Variables(_))),
                "{source}"
            );
        }
    }

    #[test]
    fn local_and_module_variable_declarators_share_exact_original_syntax_without_relabeling_scopes()
    {
        for declaration in [
            "first, [second] As Long, grid(-1 To +2,3) As Variant, pending()",
            "[True], [Me], [Rem], [Sub], [Name], [Line]",
            "untyped, typed As Object, array(0To1)",
        ] {
            let module = format!("Dim {declaration}\n");
            let local = format!("Sub Run()\nDim {declaration}\nEnd Sub\n");
            let module_inventory = parse(module.as_bytes()).unwrap();
            let local_inventory = parse(local.as_bytes()).unwrap();
            assert!(local_inventory.variables().is_empty());
            let ModuleBodyStatement::Variables(list) =
                &local_inventory.procedures()[0].statements()[0]
            else {
                panic!("local declaration list expected");
            };
            assert_eq!(list.variables().len(), module_inventory.variables().len());
            for (local_variable, module_variable) in
                list.variables().iter().zip(module_inventory.variables())
            {
                assert_eq!(
                    &local[local_variable.name()],
                    &module[module_variable.name()]
                );
                assert_eq!(
                    &local[local_variable.declarator()],
                    &module[module_variable.declarator()]
                );
                assert_eq!(
                    local_variable.type_range().map(|range| &local[range]),
                    module_variable.type_range().map(|range| &module[range])
                );
                assert_eq!(
                    local_variable.array_bounds().map(|range| &local[range]),
                    module_variable.array_bounds().map(|range| &module[range])
                );
                assert_eq!(
                    local_variable.array_work_nodes(),
                    module_variable.array_work_nodes()
                );
                assert!(module_variable.visibility().is_none());
            }
        }
    }

    #[test]
    fn local_dim_members_share_exact_aggregate_record_credits_without_inflating_logical_segments() {
        let source = b"Dim field,storage(2)\nConst Seed=7\nSub First()\nDim first,grid(-1 To +2,3) As Long,pending(),fourth\nDim typed As Long\nfirst=Seed\nCall Target(first)\nEnd Sub\nSub Second()\nDim first,second(),third As Variant\nEnd Sub\n";
        let limits = ModuleLimits::new(source.len(), 13, 2)
            .with_max_arguments(1)
            .with_max_expression_nodes(0);
        let inventory = module_inventory(source, limits, &mut || Ok::<(), ()>(())).unwrap();
        assert_eq!(inventory.statement_count(), 11);
        assert_eq!(inventory.variables().len(), 2);
        assert_eq!(inventory.constants().len(), 1);
        assert_eq!(inventory.procedures()[0].statements().len(), 4);
        assert_eq!(inventory.procedures()[1].statements().len(), 1);
        let local_count: usize = inventory
            .procedures()
            .iter()
            .flat_map(|procedure| procedure.statements())
            .map(|statement| match statement {
                ModuleBodyStatement::Variables(list) => list.variables().len(),
                ModuleBodyStatement::Declaration(_)
                | ModuleBodyStatement::Assignment(_)
                | ModuleBodyStatement::ApplicationAssignment(_)
                | ModuleBodyStatement::ExpressionAssignment(_)
                | ModuleBodyStatement::Call(_)
                | ModuleBodyStatement::ExpressionCall(_) => 1,
            })
            .sum();
        assert_eq!(
            local_count + inventory.variables().len() + inventory.constants().len(),
            13
        );
        for maximum in [0, 1, 3, 7, 10, 11, 12] {
            assert_eq!(
                module_inventory(
                    source,
                    ModuleLimits::new(source.len(), maximum, 2)
                        .with_max_arguments(1)
                        .with_max_expression_nodes(0),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::StatementLimit),
                "aggregate record ceiling {maximum}"
            );
        }
        let grouped = b"Sub Run()\nDim a,b,c,d,e,f,g,h,i,j\nEnd Sub\n";
        let inventory = module_inventory(
            grouped,
            ModuleLimits::new(grouped.len(), 10, 1),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(inventory.statement_count(), 3);
        for maximum in [0, 1, 2, 3, 5, 9] {
            assert_eq!(
                module_inventory(
                    grouped,
                    ModuleLimits::new(grouped.len(), maximum, 1),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::StatementLimit)
            );
        }
        let list_before_module = b"Sub Run()\nDim a,b,c,d,e,f,g,h,i,j\nEnd Sub\nDim after\n";
        assert!(
            module_inventory(
                list_before_module,
                ModuleLimits::new(list_before_module.len(), 11, 1),
                &mut || Ok::<(), ()>(())
            )
            .is_ok()
        );
        assert_eq!(
            module_inventory(
                list_before_module,
                ModuleLimits::new(list_before_module.len(), 10, 1),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::StatementLimit)
        );
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len() - 1, 13, 2).with_max_arguments(1),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::SourceLimit)
        );
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len(), 13, 1).with_max_arguments(1),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::ProcedureLimit)
        );
    }

    #[test]
    fn local_dim_late_malformed_or_richer_members_reject_all_preceding_module_records() {
        for tail in [
            "",
            ",other",
            "other,",
            "other,,third",
            "other As",
            "other As Product",
            "other As New Object",
            "other As String * 8",
            "other$",
            "other=1",
            "other As Long=1",
            "other(upper)",
            "other(1+2)",
            "other(,1)",
            "other(1,)",
            "other(1,,2)",
            "other(1 To)",
            "other(1",
            "other(1)(2)",
            "other(1) As Product",
            "other(1)=1",
            "holder.other",
            "[open",
            "other extra",
            "other\0",
            "other\r extra",
            "other\nthird",
        ] {
            let source = format!(
                "Dim module_field\nSub Accepted()\nDim previous, array(2) As Long\nEnd Sub\nSub Rejected()\nDim valid, second(-1 To +2,3) As Long, {tail}\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{tail:?}"
            );
        }
        for body in [
            "Public value",
            "Private value As Long",
            "Static value As Long",
            "Shared value",
            "Dim value As Product",
            "Dim value As Long=1",
            "Const first As Long=1,second As Long=2",
            "Const value=1",
            "ReDim value(2)",
        ] {
            let source = format!("Sub Run()\nDim accepted, other()\n{body}\nEnd Sub\n");
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn local_dim_module_cancellation_at_every_checkpoint_preserves_typed_failure_and_no_inventory()
    {
        let source = b"Dim module_first, module_grid(2)\nConst Seed=7\nSub First(value As Long)\nDim local, grid(-1 To +2,3) As Long, pending()\nlocal=Seed\nCall Target(local)\nEnd Sub\nSub Second()\nDim local, [local] As Variant\nEnd Sub\n";
        let limits = ModuleLimits::new(source.len(), 10, 2)
            .with_max_parameters(1)
            .with_max_arguments(1)
            .with_max_expression_nodes(0);
        let mut checkpoints = 0;
        let inventory = module_inventory(source, limits, &mut || {
            checkpoints += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(inventory.procedures().len(), 2);
        for stop in 1..=checkpoints {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source, limits, &mut || {
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
    fn body_retention_uses_total_existing_statement_limits_and_is_cancellable() {
        let source = b"Sub First()\nDim value As Long\nvalue=1\nEnd Sub\nSub Second()\nConst Limit As Long=2\nvalue=Limit\nEnd Sub\n";
        let expected = parse(source).unwrap();
        assert_eq!(expected.statement_count(), 8);
        assert_eq!(
            expected
                .procedures()
                .iter()
                .map(|procedure| procedure.statements().len())
                .sum::<usize>(),
            4
        );
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 8, 2), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Ok(expected)
        );
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 7, 2), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::StatementLimit)
        );
        let source = format!("Sub Run()\nvalue=\"{}\"\nEnd Sub\n", "x\"\"".repeat(8192));
        let mut successful_checks = 0;
        module_inventory(
            source.as_bytes(),
            ModuleLimits::new(source.len(), 3, 1),
            &mut || {
                successful_checks += 1;
                Ok::<(), ()>(())
            },
        )
        .unwrap();
        let limit = successful_checks - 2;
        let mut calls = 0;
        assert_eq!(
            module_inventory(
                source.as_bytes(),
                ModuleLimits::new(source.len(), 3, 1),
                &mut || {
                    calls += 1;
                    if calls == limit { Err("stop") } else { Ok(()) }
                }
            ),
            Err(ModuleError::Cancelled("stop"))
        );
        assert_eq!(calls, limit);
    }

    #[test]
    fn unsupported_tails_bodies_prefixes_and_module_forms_have_no_inventory() {
        for source in [
            "Sub Run(value As)\nEnd Sub",
            "Sub Run() As Long\nEnd Sub",
            "Function Value\nEnd Function",
            "Function Value() As Long()\nEnd Function",
            "Function Value$() As String\nEnd Function",
            "Function Value() As Long extra\nEnd Function",
            "Function Value() As Product\nEnd Function",
            "Function Value() As [Long]\nEnd Function",
            "Function Value() As [Types].[Result]\nEnd Function",
            "Function Value()As Long\nEnd Function",
            "Sub Run()junk\nEnd Sub",
            "Friend Sub Run()\nEnd Sub",
            "Static Public Sub Run()\nEnd Sub",
            "Public Private Sub Run()\nEnd Sub",
            "Public Static Static Sub Run()\nEnd Sub",
            "Sub Run() Rem header\nEnd Sub",
            "Sub Run()\nEnd Sub Rem tail",
            "Sub Run()\nSub Nested()\nEnd Sub\nEnd Sub",
            "Sub Run()\nEnd Function",
            "Sub Run()",
            "End Sub",
            "Public",
            "Property Get Value()\nEnd Property",
            "Declare Sub Run Lib \"host\"()",
            "#If Enabled Then\nSub Run()\nEnd Sub\n#End If",
            "Dim value As Product",
            "Attribute VB_Name = \"Main\"\nAttribute VB_Description = \"note\"",
            "Attribute VB_Name = \"Main\"\nAttribute VB_Name = \"Other\"",
            "Option Explicit\nAttribute VB_Name = \"Main\"",
            "Sub Run()\nEnd Sub\nOption Explicit",
            "Sub Run()\nEnd Sub\nAttribute VB_Name = \"Main\"",
            "Option Base 2",
            "Option Compare Unknown",
            "Option Private Class",
            "Option Explicit extra",
            "Option Base 1\nOption Base 0",
            "Option Explicit\nOption Explicit",
            "Attribute VB_Name = \"\"",
            "Attribute VB_Name = \"1Main\"",
            "Attribute VB_Name = \"Main Name\"",
            "Attribute VB_Name = \"_Main\"",
            "Attribute VB_Name = \"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef\"",
            "Attribute VB_Name = \"Main\" extra",
            "Sub [Run]()\nEnd Sub\nunknown",
            "Rem-x",
            "Rem: Sub Run()\nEnd Sub",
            "' continued _\nSub Run()\nEnd Sub",
            "Rem continued _\r\nSub Run()\r\nEnd Sub",
            "Sub Run() ' continued _\nEnd Sub",
            "Sub Rem\nEnd Sub",
            "Sub Rem()\nEnd Sub",
            "Sub Rem ()\nEnd Sub",
            "Function Rem()\nEnd Function",
            "Function Rem!()\nEnd Function",
        ] {
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn lexical_failures_never_accept_a_valid_prefix() {
        for source in [
            b"Sub Run()\rEnd Sub".as_slice(),
            b"Sub Run()\nEnd Sub\0",
            b"' comment\0",
            b"Sub [Run()\nEnd Sub",
            b"Attribute VB_Name = \"Main",
            b"Attribute VB_Name = \"Main _\n",
            b"Sub Run(\n)\nEnd Sub",
        ] {
            assert!(
                matches!(parse(source), Err(ModuleError::Unsupported { .. })),
                "{source:?}"
            );
        }
        let source = b"Sub Run()\nEnd Sub\nUnknown";
        for end in 1..source.len() {
            let prefix = &source[..end];
            if let Ok(inventory) = parse(prefix) {
                assert!(
                    inventory.procedures().is_empty()
                        || prefix.ends_with(b"End Sub")
                        || prefix.ends_with(b"End Sub\n")
                );
            }
        }
        for byte in 0..=u8::MAX {
            let source = [b"Sub Run".as_slice(), &[byte], b"()\nEnd Sub"].concat();
            if let Ok(inventory) = parse(&source) {
                for procedure in inventory.procedures() {
                    for range in [
                        procedure.declaration(),
                        procedure.header(),
                        procedure.name(),
                        procedure.body(),
                        procedure.terminator(),
                    ] {
                        assert!(range.start <= range.end && range.end <= source.len());
                    }
                }
            }
        }
    }

    #[test]
    fn limits_are_exact_and_include_trivia_segments() {
        let source = b"' comment\n\nSub Run():End Sub";
        let inventory = parse(source).unwrap();
        assert_eq!(inventory.statement_count(), 4);
        assert!(
            module_inventory(source, ModuleLimits::new(source.len(), 4, 1), &mut || Ok::<
                (),
                (),
            >(
                ()
            ))
            .is_ok()
        );
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len() - 1, 4, 1),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::SourceLimit)
        );
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 3, 1), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::StatementLimit)
        );
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 4, 0), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::ProcedureLimit)
        );
        assert!(
            module_inventory(b"", ModuleLimits::new(0, 0, 0), &mut || Ok::<(), ()>(())).is_ok()
        );
        assert_eq!(
            module_inventory(b"\n", ModuleLimits::new(1, 0, 0), &mut || Ok::<(), ()>(())),
            Err(ModuleError::StatementLimit)
        );
    }

    #[test]
    fn scans_propagate_initial_and_mid_scan_cancellation() {
        assert_eq!(
            module_inventory(b"", ModuleLimits::new(0, 0, 0), &mut || Err("stop")),
            Err(ModuleError::Cancelled("stop"))
        );
        for source in [
            format!("'{}\n", "x".repeat(8192)),
            format!("Sub {}()\nEnd Sub", "x".repeat(8192)),
            format!("Public {}Sub Run()\nEnd Sub", " _\n".repeat(8192)),
        ] {
            let mut calls = 0;
            assert_eq!(
                module_inventory(
                    source.as_bytes(),
                    ModuleLimits::new(source.len(), source.len(), 1),
                    &mut || {
                        calls += 1;
                        if calls == 8 { Err("stop") } else { Ok(()) }
                    }
                ),
                Err(ModuleError::Cancelled("stop"))
            );
            assert_eq!(calls, 8);
        }
    }

    #[test]
    fn application_assignments_preserve_module_relative_ranges_trivia_and_procedure_ownership() {
        for newline in ["\n", "\r\n"] {
            let parentheses = format!(
                "( _{newline} 007 _{newline}, _{newline} \"a,:='Rem End Sub \"\"quoted\"\"\", [local], FALSE _{newline} )"
            );
            let application = format!("[Read] _{newline} {parentheses}");
            let assignment_text =
                format!("lEt _{newline} [local] _{newline} = _{newline} {application}");
            let source = format!(
                "Attribute VB_Name = \"ApplicationModule\"{newline}Public values(2) As Long{newline}Sub First(){newline}  Dim local As Long{newline}  {assignment_text} ' End Sub: ignored(){newline}  local=Read():Call Finish(1):End Sub{newline}Function NextValue() As Long{newline}NextValue=[Read](False){newline}End Function{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(inventory.procedures().len(), 2);
            let first = &inventory.procedures()[0];
            assert_eq!(first.statements().len(), 4);
            let ModuleBodyStatement::ApplicationAssignment(assignment) = &first.statements()[1]
            else {
                panic!("application assignment retained after local declaration")
            };
            assert_eq!(&source[assignment.statement()], assignment_text);
            assert_eq!(&source[assignment.destination()], "[local]");
            assert_eq!(&source[assignment.application()], application);
            assert_eq!(&source[assignment.callee()], "[Read]");
            assert_eq!(&source[assignment.arguments()], parentheses);
            assert_eq!(assignment.argument_values().len(), 4);
            for (value, (spelling, kind)) in assignment.argument_values().iter().zip([
                ("007", ModuleScalarValueKind::UnsignedDecimal),
                (
                    "\"a,:='Rem End Sub \"\"quoted\"\"\"",
                    ModuleScalarValueKind::String,
                ),
                ("[local]", ModuleScalarValueKind::Name),
                ("FALSE", ModuleScalarValueKind::Boolean),
            ]) {
                assert_eq!(&source[value.span()], spelling);
                assert_eq!(value.kind(), kind);
                assert!(assignment.arguments().start < value.span().start);
                assert!(value.span().end < assignment.arguments().end);
            }
            let ModuleBodyStatement::ApplicationAssignment(empty) = &first.statements()[2] else {
                panic!("empty application retained before explicit Call")
            };
            assert_eq!(&source[empty.arguments()], "()");
            assert!(empty.argument_values().is_empty());
            let second = &inventory.procedures()[1];
            let ModuleBodyStatement::ApplicationAssignment(result) = &second.statements()[0] else {
                panic!("function result destination retained without binding")
            };
            assert_eq!(&source[result.destination()], "NextValue");
            assert_eq!(&source[result.application()], "[Read](False)");
            for procedure in inventory.procedures() {
                for statement in procedure.statements() {
                    assert!(procedure.body().start <= statement.statement().start);
                    assert!(statement.statement().end <= procedure.body().end);
                }
            }
        }
    }

    #[test]
    fn application_and_call_arguments_share_exact_aggregate_quotas_across_procedures() {
        let source = b"Sub First():value=Read(1,True):Call Finish(\"text\"):value=Read():End Sub\nSub Second()\nLet value=Read([value],False)\nCall Finish\nEnd Sub\n";
        let expected = parse(source).unwrap();
        assert_eq!(expected.statement_count(), 9);
        assert_eq!(expected.procedures()[0].statements().len(), 3);
        assert_eq!(expected.procedures()[1].statements().len(), 2);
        let limits = ModuleLimits::new(source.len(), 9, 2).with_max_arguments(5);
        assert_eq!(
            module_inventory(source, limits, &mut || Ok::<(), ()>(())),
            Ok(expected)
        );
        for maximum in 0..5 {
            assert_eq!(
                module_inventory(source, limits.with_max_arguments(maximum), &mut || Ok::<
                    (),
                    (),
                >(
                    ()
                )),
                Err(ModuleError::ArgumentLimit),
                "argument ceiling {maximum}"
            );
        }
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len() - 1, 9, 2).with_max_arguments(5),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::SourceLimit)
        );
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len(), 8, 2).with_max_arguments(5),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::StatementLimit)
        );
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len(), 9, 1).with_max_arguments(5),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::ProcedureLimit)
        );
        let zero = b"Sub Run():value=Read():Call Finish():Call Finish:End Sub\n";
        let inventory = module_inventory(
            zero,
            ModuleLimits::new(zero.len(), 5, 1).with_max_arguments(0),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(inventory.procedures()[0].statements().len(), 3);
        let ModuleBodyStatement::ApplicationAssignment(empty) =
            &inventory.procedures()[0].statements()[0]
        else {
            panic!("empty application does not consume argument quota")
        };
        assert!(empty.argument_values().is_empty());
    }

    #[test]
    fn application_argument_large_lists_are_retained_and_charged_before_later_calls() {
        let values = (0..128)
            .map(|number| number.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let source =
            format!("Sub Run()\nvalue=Read({values})\nCall Finish(1,True,False)\nEnd Sub\n");
        let limits = ModuleLimits::new(source.len(), 4, 1).with_max_arguments(131);
        let inventory =
            module_inventory(source.as_bytes(), limits, &mut || Ok::<(), ()>(())).unwrap();
        let ModuleBodyStatement::ApplicationAssignment(assignment) =
            &inventory.procedures()[0].statements()[0]
        else {
            panic!("many application arguments retained")
        };
        assert_eq!(assignment.argument_values().len(), 128);
        for (index, value) in assignment.argument_values().iter().enumerate() {
            assert_eq!(&source[value.span()], index.to_string());
        }
        for maximum in [0, 1, 127, 128, 130] {
            assert_eq!(
                module_inventory(
                    source.as_bytes(),
                    limits.with_max_arguments(maximum),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::ArgumentLimit),
                "argument ceiling {maximum}"
            );
        }
    }

    #[test]
    fn application_assignment_late_invalid_tails_reject_every_preceding_module_record() {
        for body in [
            "value=Read(1,)",
            "value=Read(,1)",
            "value=Read(1,,2)",
            "value=Read(1,next:=2)",
            "value=Read(1,ByVal next)",
            "value=Read(1,next())",
            "value=Read(1,next.member)",
            "value=Read(1,(next))",
            "value=Read(1,-2.0)",
            "value=Read(1,+&H2)",
            "value=Read(1,2.0)",
            "value=Read(1,2E3)",
            "value=Read(1,#1/1/2000#)",
            "value=Read(1,next$)",
            "value=Read(1) extra",
            "value=Read(1)()",
            "value=Read(1).member",
            "value=Read(1)+&H2",
            "value=Read 1,2",
            "value=holder.Read(1)",
            "value(1)=Read()",
            "value=Read(1,\"open",
            "value=Read(1,\"continued _\n",
            "value=Read(1, ' inline\n2)",
            "value=Read(1\n)",
            "value=Read(1,\0)",
            "value=Read(1,\r2)",
            "Const Local As Long=Read()",
            "Dim local As Long=Read()",
            "value=Read() Rem tail",
        ] {
            let source = format!(
                "Const Seed=7\nSub Accepted()\nprevious=Read(1)\nEnd Sub\nSub Rejected()\nvalue=Read(True)\n{body}\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{body:?}"
            );
        }
        for source in [
            "Const Seed=Read()\nSub Run()\nEnd Sub\n",
            "Sub Run(Optional value=Read())\nEnd Sub\n",
            "Sub Run(Optional value As Long=Read(1))\nEnd Sub\n",
            "Sub Run(value=Read())\nEnd Sub\n",
            "value=Read()\nSub Run()\nEnd Sub\n",
            "Sub Run()\nEnd Sub\nvalue=Read()\n",
        ] {
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn application_assignment_module_cancellation_never_returns_a_partial_inventory() {
        let source = format!(
            "Const Seed=7\nSub First(value)\nresult=Read({},{},\"{}\" _\r\n,{}True)\nCall Finish(Seed)\nEnd Sub\nSub Second()\nLet result=[Read](False)\nEnd Sub\n",
            "9".repeat(512),
            "value".repeat(128),
            "x\"\"".repeat(256),
            " _\n".repeat(16)
        );
        let limits = ModuleLimits::new(source.len(), 8, 2)
            .with_max_parameters(1)
            .with_max_arguments(6);
        let mut checkpoints = 0;
        let inventory = module_inventory(source.as_bytes(), limits, &mut || {
            checkpoints += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(inventory.procedures().len(), 2);
        assert_eq!(inventory.procedures()[0].statements().len(), 2);
        for cancelled_at in 1..=checkpoints {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source.as_bytes(), limits, &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err(ModuleError::Cancelled("stop")),
                "module checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
    }

    #[test]
    fn application_assignment_module_all_byte_suffixes_preserve_comment_and_colon_boundaries() {
        for byte in 0..=u8::MAX {
            let source = [
                b"Sub Run()\nvalue=Read(1)".as_slice(),
                &[byte],
                b"\nEnd Sub\n",
            ]
            .concat();
            let result = parse(&source);
            let expected = matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | b'\'' | b':');
            assert_eq!(result.is_ok(), expected, "module suffix byte {byte}");
            if let Ok(inventory) = result {
                assert_eq!(inventory.procedures()[0].statements().len(), 1);
                let ModuleBodyStatement::ApplicationAssignment(assignment) =
                    &inventory.procedures()[0].statements()[0]
                else {
                    panic!("original application retained before trivia boundary")
                };
                assert_eq!(&source[assignment.statement()], b"value=Read(1)");
                assert_eq!(&source[assignment.arguments()], b"(1)");
                assert_eq!(&source[assignment.argument_values()[0].span()], b"1");
                assert_eq!(inventory.source_len(), source.len());
            }
        }
    }

    #[test]
    fn expression_assignments_preserve_source_ranges_nested_inventory_and_old_fast_paths() {
        for newline in ["\n", "\r\n"] {
            let expression =
                format!("[Outer] _{newline} (value _{newline} + 1, [Inner](other)) * -2");
            let written = format!("Let [sink] = {expression}");
            let source = format!(
                "Const Seed=7{newline}Sub Run(){newline}  {written} ' End Sub: ignored(){newline}sink=1:sink=Ready(2):Call Finish(3){newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            let statements = inventory.procedures()[0].statements();
            assert_eq!(statements.len(), 4);
            let ModuleBodyStatement::ExpressionAssignment(assignment) = &statements[0] else {
                panic!("nested structured expression retained")
            };
            assert_eq!(&source[assignment.statement()], written);
            assert_eq!(&source[assignment.destination()], "[sink]");
            assert_eq!(&source[assignment.expression()], expression);
            assert_eq!(assignment.inventory().work_nodes(), 11);
            assert_eq!(assignment.inventory().argument_count(), 3);
            assert_eq!(
                assignment
                    .inventory()
                    .values()
                    .iter()
                    .map(|value| &source[value.span()])
                    .collect::<Vec<_>>(),
                ["value", "1", "other", "2"]
            );
            assert_eq!(
                assignment
                    .inventory()
                    .applications()
                    .iter()
                    .map(|application| &source[application.callee()])
                    .collect::<Vec<_>>(),
                ["[Outer]", "[Inner]"]
            );
            for application in assignment.inventory().applications() {
                assert!(assignment.expression().start <= application.application().start);
                assert!(application.application().end <= assignment.expression().end);
                for argument in application.argument_expressions() {
                    assert!(
                        application.arguments().start < argument.start
                            && argument.end < application.arguments().end
                    );
                }
            }
            assert!(matches!(statements[1], ModuleBodyStatement::Assignment(_)));
            assert!(matches!(
                statements[2],
                ModuleBodyStatement::ApplicationAssignment(_)
            ));
            assert!(matches!(statements[3], ModuleBodyStatement::Call(_)));
        }
    }

    #[test]
    fn expression_calls_preserve_whole_module_ranges_comments_and_procedure_ownership() {
        for newline in ["\n", "\r\n"] {
            let parentheses = format!("(value _{newline} + 1, [Inner](other) * -2)");
            let written = format!("Call _{newline} [Target] _{newline} {parentheses}");
            let source = format!(
                "Attribute VB_Name = \"CallModule\"{newline}Const Seed=7{newline}Sub Run(){newline}  Dim local As Long{newline}  {written} ' End Sub: ignored(){newline}Call Target(1+2):Call Finish(3):End Sub{newline}Sub Second(){newline}Call Finish{newline}End Sub{newline}"
            );
            let inventory = parse(source.as_bytes()).unwrap();
            assert_eq!(inventory.procedures().len(), 2);
            let procedure = &inventory.procedures()[0];
            let statements = procedure.statements();
            assert_eq!(statements.len(), 4);
            let ModuleBodyStatement::ExpressionCall(call) = &statements[1] else {
                panic!("nested explicit expression Call retained after local declaration");
            };
            assert_eq!(&source[call.statement()], written);
            assert_eq!(&source[call.callee()], "[Target]");
            assert_eq!(&source[call.arguments()], parentheses);
            assert_eq!(call.inventory().work_nodes(), 11);
            assert_eq!(call.inventory().argument_count(), 3);
            assert_eq!(call.argument_expressions().len(), 2);
            assert_eq!(
                call.inventory()
                    .applications()
                    .iter()
                    .map(|application| &source[application.callee()])
                    .collect::<Vec<_>>(),
                ["[Target]", "[Inner]"]
            );
            for value in call.inventory().values() {
                assert!(procedure.body().start <= value.span().start);
                assert!(value.span().end <= procedure.body().end);
                assert!(call.arguments().start < value.span().start);
                assert!(value.span().end < call.arguments().end);
            }
            let ModuleBodyStatement::ExpressionCall(formerly_unsupported) = &statements[2] else {
                panic!("binary argument Call retained at a colon boundary");
            };
            assert_eq!(
                &source[formerly_unsupported.statement()],
                "Call Target(1+2)"
            );
            assert_eq!(
                &source[formerly_unsupported.argument_expressions()[0].clone()],
                "1+2"
            );
            assert!(matches!(statements[0], ModuleBodyStatement::Declaration(_)));
            assert!(matches!(statements[3], ModuleBodyStatement::Call(_)));
            assert!(matches!(
                inventory.procedures()[1].statements()[0],
                ModuleBodyStatement::Call(_)
            ));
            for statement in statements {
                assert!(procedure.body().start <= statement.statement().start);
                assert!(statement.statement().end <= procedure.body().end);
            }
        }
    }

    #[test]
    fn expression_call_work_and_arguments_share_aggregate_limits_with_assignments_and_old_calls() {
        let source = b"Sub First()\nCall Ready(1)\nsink=Ready(2)\nsink=1+2\nCall Target(value+1,Inner(other)*-2)\nEnd Sub\nSub Second()\nsink=B(C(3))\nCall Finish\nEnd Sub\n";
        let limits = ModuleLimits::new(source.len(), 10, 2)
            .with_max_arguments(7)
            .with_max_expression_nodes(19);
        let inventory = module_inventory(source, limits, &mut || Ok::<(), ()>(())).unwrap();
        assert_eq!(inventory.statement_count(), 10);
        assert_eq!(inventory.procedures().len(), 2);
        let statements = inventory.procedures()[0].statements();
        assert!(matches!(statements[0], ModuleBodyStatement::Call(_)));
        assert!(matches!(
            statements[1],
            ModuleBodyStatement::ApplicationAssignment(_)
        ));
        assert!(matches!(
            statements[2],
            ModuleBodyStatement::ExpressionAssignment(_)
        ));
        let ModuleBodyStatement::ExpressionCall(call) = &statements[3] else {
            panic!("expression Call retained among mixed statements");
        };
        assert_eq!(call.inventory().work_nodes(), 11);
        assert_eq!(call.inventory().argument_count(), 3);
        for maximum in [0, 1, 2, 4, 5, 6] {
            assert_eq!(
                module_inventory(source, limits.with_max_arguments(maximum), &mut || Ok::<
                    (),
                    (),
                >(
                    ()
                )),
                Err(ModuleError::ArgumentLimit),
                "aggregate argument ceiling {maximum}"
            );
        }
        for maximum in [0, 1, 3, 13, 14, 18] {
            assert_eq!(
                module_inventory(
                    source,
                    limits.with_max_expression_nodes(maximum),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::ExpressionLimit),
                "aggregate expression ceiling {maximum}"
            );
        }
        for (limits, expected) in [
            (
                ModuleLimits::new(source.len() - 1, 10, 2),
                ModuleError::SourceLimit,
            ),
            (
                ModuleLimits::new(source.len(), 9, 2),
                ModuleError::StatementLimit,
            ),
            (
                ModuleLimits::new(source.len(), 10, 1),
                ModuleError::ProcedureLimit,
            ),
        ] {
            assert_eq!(
                module_inventory(
                    source,
                    limits.with_max_arguments(7).with_max_expression_nodes(19),
                    &mut || Ok::<(), ()>(())
                ),
                Err(expected)
            );
        }
        let old = b"Sub Run()\nsink=1\nsink=Ready(2)\nCall Ready(3)\nCall Finish\nCall Finish()\nEnd Sub\n";
        let inventory = module_inventory(
            old,
            ModuleLimits::new(old.len(), 7, 1)
                .with_max_arguments(2)
                .with_max_expression_nodes(0),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert!(
            inventory.procedures()[0]
                .statements()
                .iter()
                .all(|statement| matches!(
                    statement,
                    ModuleBodyStatement::Assignment(_)
                        | ModuleBodyStatement::ApplicationAssignment(_)
                        | ModuleBodyStatement::Call(_)
                ))
        );
    }

    #[test]
    fn expression_call_late_unsupported_tail_rejects_the_whole_module_without_prefix_records() {
        for tail in [
            "Call Target(1+2)+3",
            "Call (Target(1+2))",
            "Call Target(1+2)(3)",
            "Call Target(1+)",
            "Call Target(1+2,)",
            "Call Target(value:=1+2)",
            "Call Target(ByVal value+1)",
            "Call Target(Inner(value:=1))",
            "Call Target(Inner(1).member)",
            "Call Target(Line(1)+2)",
            "Call Target(2^3)",
            "Call Target(a&b)",
            "Call Target(1.0+2)",
            "Call Target(&H10+2)",
            "Call Target(value$+1)",
            "Call Target(1+2) extra",
            "Call Target value+1",
            "Call Name(1+2)",
            "Call\x0cTarget(1+2)",
            "Call Target(1\0+2)",
            "Call Target(1\r+2)",
            "Call Target(\"physical\nline\" & value)",
            "Const Local As Long=1+2",
            "Dim local As Long=Inner(1)+2",
        ] {
            let source = format!(
                "Const Seed=7\nSub Accepted()\nCall Target(value+1,Inner(other)*-2)\nEnd Sub\nSub Rejected()\nsink=Outer(1+2)\n{tail}\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{tail:?}"
            );
        }
        for source in [
            "Call Target(1+2)\nSub Run()\nEnd Sub\n",
            "Sub Run(Optional value=1+2)\nCall Target(1+2)\nEnd Sub\n",
            "Const Seed=1+2\nSub Run()\nCall Target(1+2)\nEnd Sub\n",
        ] {
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn expression_call_module_cancellation_at_every_checkpoint_never_publishes_partial_records() {
        let source = b"Const Seed=7\nSub First(value As Long)\nsink=1+2\nCall Target((value+007),Inner(\"a\"\"b\" & [True]))\nEnd Sub\nSub Second()\nCall Finish(-1)\nEnd Sub\n";
        let limits = ModuleLimits::new(source.len(), 8, 2)
            .with_max_parameters(1)
            .with_max_arguments(4);
        let mut checkpoints = 0;
        let inventory = module_inventory(source, limits, &mut || {
            checkpoints += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(inventory.procedures().len(), 2);
        assert!(matches!(
            inventory.procedures()[0].statements()[1],
            ModuleBodyStatement::ExpressionCall(_)
        ));
        for cancelled_at in 1..=checkpoints {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source, limits, &mut || {
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
    fn expression_limits_are_aggregate_exact_short_zero_and_independent_of_old_records() {
        let source = b"Sub First()\nsink=1+2\nEnd Sub\nSub Second()\nsink=3*4\nEnd Sub\n";
        let limits = ModuleLimits::new(source.len(), 6, 2)
            .with_max_arguments(0)
            .with_max_expression_nodes(6);
        let inventory = module_inventory(source, limits, &mut || Ok::<(), ()>(())).unwrap();
        assert_eq!(inventory.statement_count(), 6);
        for maximum in [0, 1, 2, 3, 5] {
            assert_eq!(
                module_inventory(
                    source,
                    limits.with_max_expression_nodes(maximum),
                    &mut || Ok::<(), ()>(())
                ),
                Err(ModuleError::ExpressionLimit),
                "node ceiling {maximum}"
            );
        }
        assert_eq!(
            module_inventory(
                source,
                ModuleLimits::new(source.len() - 1, 6, 2),
                &mut || Ok::<(), ()>(())
            ),
            Err(ModuleError::SourceLimit)
        );
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 5, 2), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::StatementLimit)
        );
        assert_eq!(
            module_inventory(source, ModuleLimits::new(source.len(), 6, 1), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::ProcedureLimit)
        );
        let old = b"Const Seed=7\nSub Run()\nsink=1\nsink=Ready()\nCall Finish\nEnd Sub\n";
        let old_inventory = module_inventory(
            old,
            ModuleLimits::new(old.len(), 6, 1)
                .with_max_arguments(0)
                .with_max_expression_nodes(0),
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert!(matches!(
            old_inventory.procedures()[0].statements()[0],
            ModuleBodyStatement::Assignment(_)
        ));
        assert!(matches!(
            old_inventory.procedures()[0].statements()[1],
            ModuleBodyStatement::ApplicationAssignment(_)
        ));
        assert!(matches!(
            old_inventory.procedures()[0].statements()[2],
            ModuleBodyStatement::Call(_)
        ));
    }

    #[test]
    fn expression_arguments_share_exact_module_quota_with_call_and_atomic_application_records() {
        let source = b"Sub First()\nCall Ready(1)\nsink=Ready(2)\nsink=Outer(value+1,Inner(other))*-2\nEnd Sub\nSub Second()\nsink=B(C(3))\nEnd Sub\n";
        let limits = ModuleLimits::new(source.len(), 8, 2)
            .with_max_arguments(7)
            .with_max_expression_nodes(16);
        let inventory = module_inventory(source, limits, &mut || Ok::<(), ()>(())).unwrap();
        assert_eq!(inventory.procedures().len(), 2);
        assert_eq!(inventory.statement_count(), 8);
        for maximum in [0, 1, 4, 5, 6] {
            assert_eq!(
                module_inventory(source, limits.with_max_arguments(maximum), &mut || Ok::<
                    (),
                    (),
                >(
                    ()
                )),
                Err(ModuleError::ArgumentLimit),
                "argument ceiling {maximum}"
            );
        }
        assert_eq!(
            module_inventory(source, limits.with_max_expression_nodes(15), &mut || Ok::<
                (),
                (),
            >(
                ()
            )),
            Err(ModuleError::ExpressionLimit)
        );
    }

    #[test]
    fn expression_late_malformed_or_richer_source_rejects_the_whole_module() {
        for tail in [
            "sink=1+",
            "sink=(1+2",
            "sink=1+2)",
            "sink=F(1,)",
            "sink=F(1,,2)",
            "sink=F(value:=1)",
            "sink=F(ByVal value)",
            "sink=F(G(1).member)",
            "sink=F(1)(2)",
            "sink=F(1)+&H2",
            "sink=F(1)+2.0",
            "sink=1E2+3",
            "sink=#1/1/2000#",
            "sink=value$+1",
            "sink=Me+1",
            "sink=Line(1)+2",
            "sink=a<b<",
            "sink=\"open",
            "sink=\"physical\nline\"",
            "sink(1)=a+b",
            "item.member=a+b",
            "sink=1\r+2",
            "sink=1\0+2",
            "sink=1+2 extra",
            "Const Local As Long=1+2",
            "Dim local As Long=F(1)+2",
        ] {
            let source = format!(
                "Const Seed=7\nDim storage(2)\nSub First()\nsink=1+2\nEnd Sub\nSub Rejected()\nsink=Outer(1+2)\n{tail}\nEnd Sub\n"
            );
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{tail:?}"
            );
        }
        for source in [
            "Const Seed=1+2\nSub Run()\nEnd Sub\n",
            "Sub Run(Optional value=F(1))\nEnd Sub\n",
            "Sub Run(Optional value=1+2)\nEnd Sub\n",
            "sink=1+2\nSub Run()\nEnd Sub\n",
        ] {
            assert!(
                matches!(
                    parse(source.as_bytes()),
                    Err(ModuleError::Unsupported { .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn expression_module_cancellation_at_every_checkpoint_never_exposes_a_partial_inventory() {
        let source = b"Const Seed=7\nSub First()\nsink=(Outer(value+1,Inner(\"a\"\"b\"&[True])))*-2\nEnd Sub\nSub Second()\nsink=3+4\nEnd Sub\n";
        let limits = ModuleLimits::new(source.len(), 7, 2).with_max_arguments(3);
        let mut checkpoints = 0;
        module_inventory(source, limits, &mut || {
            checkpoints += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
        for cancelled_at in 1..=checkpoints {
            let mut calls = 0;
            assert_eq!(
                module_inventory(source, limits, &mut || {
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
    fn expression_module_all_byte_suffixes_preserve_complete_comment_colon_and_crlf_boundaries() {
        for byte in 0..=u8::MAX {
            let source = [b"Sub Run()\nsink=(1+2)".as_slice(), &[byte], b"\nEnd Sub\n"].concat();
            let result = parse(&source);
            let expected = matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | b'\'' | b':');
            assert_eq!(
                result.is_ok(),
                expected,
                "expression module suffix byte {byte}"
            );
            if let Ok(inventory) = result {
                assert_eq!(inventory.procedures()[0].statements().len(), 1);
                let ModuleBodyStatement::ExpressionAssignment(assignment) =
                    &inventory.procedures()[0].statements()[0]
                else {
                    panic!("structured expression retained before trivia boundary")
                };
                assert_eq!(&source[assignment.statement()], b"sink=(1+2)");
                assert_eq!(&source[assignment.expression()], b"(1+2)");
                assert_eq!(assignment.inventory().work_nodes(), 4);
            }
        }
    }

    #[test]
    fn expression_typed_suffixes_reject_the_whole_module_without_relabeling_source_atoms() {
        for (atom, suffixes) in [("2", b"%&^!#@".as_slice()), ("value", b"%&^!#@$")] {
            for suffix in suffixes {
                let source = format!(
                    "Const Seed=7\nSub First()\nsink=1+2\nEnd Sub\nSub Rejected()\nsink={atom}{}3\nEnd Sub\n",
                    char::from(*suffix)
                );
                assert!(
                    matches!(
                        parse(source.as_bytes()),
                        Err(ModuleError::Unsupported { .. })
                    ),
                    "{source}"
                );
            }
        }
        for expression in [
            "2 ^ 3",
            "2 & 3",
            "value ^ other",
            "value & other",
            "[value]^[other]",
            "[value]&[other]",
            "(value)^other",
            "Read(1)^2",
            "\"a\"&\"b\"",
        ] {
            let source = format!("Sub Run()\nsink={expression}\nEnd Sub\n");
            let inventory = parse(source.as_bytes()).unwrap();
            let ModuleBodyStatement::ExpressionAssignment(assignment) =
                &inventory.procedures()[0].statements()[0]
            else {
                panic!("unambiguous expression remains supported")
            };
            assert_eq!(&source[assignment.expression()], expression);
        }
    }
}
