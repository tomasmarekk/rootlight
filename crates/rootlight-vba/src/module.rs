//! Inventories a closed procedural VBA module without a parser runtime.
//! Acceptance covers the entire input; an unsupported statement cannot expose
//! a partial structural inventory for an adapter to mistake for complete facts.

use std::{fmt, ops::Range};

use super::{ModuleBodyStatement, ProcedureKind, logical_whitespace_len, procedure_header};

/// Caller-owned ceilings for one complete module scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleLimits {
    max_source_bytes: usize,
    max_statements: usize,
    max_procedures: usize,
    max_parameters: usize,
    max_arguments: usize,
}

impl ModuleLimits {
    /// Creates source-byte, logical-statement and retained-procedure ceilings.
    /// Zero ceilings are valid and admit only inputs requiring no such resource.
    /// Aggregate parameter and argument ceilings default to the procedure ceiling;
    /// use [`Self::with_max_parameters`] and [`Self::with_max_arguments`] to admit
    /// separately bounded comma lists.
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

    /// Sets the aggregate retained-argument ceiling across every Call in the module.
    /// A logical statement can contain many arguments, so this ceiling is separate
    /// from statement count. Zero still admits bare and explicitly empty Calls.
    #[must_use]
    pub const fn with_max_arguments(mut self, max_arguments: usize) -> Self {
        self.max_arguments = max_arguments;
        self
    }
}

/// A complete structural inventory for the supported procedural module slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleInventory {
    source_len: usize,
    module_name: Option<Range<usize>>,
    procedures: Vec<ModuleProcedure>,
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

    /// Returns consumed logical segments, including blank and comment segments.
    /// A trailing EOF does not add a synthetic segment.
    #[must_use]
    pub const fn statement_count(&self) -> usize {
        self.statement_count
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

/// One explicitly intrinsic-typed scalar parameter in original source bytes.
/// The closed slice excludes defaults, array bounds and type suffixes. Its ranges
/// are relative to the entire module, never a reconstructed parameter list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleParameter {
    declaration: Range<usize>,
    name: Range<usize>,
    type_range: Range<usize>,
    passing_mode: Option<ModuleParameterPassingMode>,
}

impl ModuleParameter {
    /// Returns the written modifier or name through the intrinsic type keyword.
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

    /// Returns the intrinsic scalar type keyword without preceding `As` trivia.
    #[must_use]
    pub fn type_range(&self) -> Range<usize> {
        self.type_range.clone()
    }

    /// Returns the written passing modifier, or None when no modifier is written.
    /// Omission is retained as source evidence rather than inferred as `ByRef`.
    #[must_use]
    pub const fn passing_mode(&self) -> Option<ModuleParameterPassingMode> {
        self.passing_mode
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
    /// Consumed logical segments exceed the caller's statement ceiling.
    StatementLimit,
    /// Declared procedures exceed the caller's retained-procedure ceiling.
    ProcedureLimit,
    /// Retained parameters across all procedures exceed the parameter ceiling.
    ParameterLimit,
    /// Retained Call arguments across the module exceed the argument ceiling.
    ArgumentLimit,
    /// Fallible reservation of bounded procedure, parameter, argument or body storage failed.
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
/// empty or contain a complete comma list of scalar parameters with required
/// intrinsic `As` types and optional written `ByVal`/`ByRef`. Defaults, arrays, `Optional`,
/// `ParamArray` and type-suffixed parameter names are unsupported. Functions retain
/// an optional intrinsic scalar As return clause or unbracketed type suffix.
/// Public/Private and optional Static modifiers are accepted in that order.
/// Bodies admit one typed scalar Dim or Const per statement and simple scalar
/// assignments with an optional Let prefix. Const values are unsigned decimal,
/// physically closed doubled-quote strings or True/False; assignments additionally
/// admit a simple name. Explicit Call statements admit a simple callee and optional
/// parentheses containing a complete positional list of these scalar atoms, with
/// space/tab-based logical trivia. Named, omitted, ByVal and nonparenthesized
/// arguments are unsupported. Keyword-shaped
/// unbracketed names, other calls and operators are unsupported. Every body
/// statement must be consumed completely.
/// Bare `Name` callees stay outside this closed call subset; bracketed `[Name]` is admitted.
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
/// Across all procedures, retained body records cannot exceed the statement ceiling.
/// Retained parameters across all procedures have a separate aggregate ceiling;
/// their storage is reserved fallibly, never inferred from logical statement count.
/// Retained Call arguments have their own aggregate ceiling across every procedure,
/// with fallible storage; positional atoms do not establish parameter or type identity.
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
        statement_count: 0,
    };
    let mut open: Option<ModuleProcedure> = None;
    let mut cursor = 0;
    let mut options = 0_u8;
    let mut attribute_header = true;
    let mut body_count = 0_usize;
    let mut parameter_count = 0_usize;
    let mut argument_count = 0_usize;
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
                let Some(statement) = super::module_body::statement(
                    source,
                    range.clone(),
                    remaining_arguments,
                    check,
                )?
                else {
                    return Err(ModuleError::Unsupported { range });
                };
                let retained_arguments = match &statement {
                    ModuleBodyStatement::Call(call) => call.argument_values().len(),
                    ModuleBodyStatement::Declaration(_) | ModuleBodyStatement::Assignment(_) => 0,
                };
                argument_count = argument_count
                    .checked_add(retained_arguments)
                    .ok_or(ModuleError::ArgumentLimit)?;
                let maximum = limits.max_statements - body_count + procedure.statements.len();
                super::module_body::reserve(&mut procedure.statements, maximum)?;
                procedure.statements.push(statement);
                body_count += 1;
            }
            continue;
        }
        if inventory.procedures.is_empty() {
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
            parameter.type_range =
                range.start + parameter.type_range.start..range.start + parameter.type_range.end;
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
        cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)
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
        *cursor += logical_whitespace_len(text.get(*cursor..).unwrap_or_default(), check)
            .map_err(ModuleError::Cancelled)?;
        match text.get(*cursor) {
            Some(b')') => return Ok(Some(parameters)),
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

fn scalar_parameter<E>(
    text: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleParameter>, E> {
    check()?;
    let start = *cursor;
    let modifier = word(text, cursor, check)?;
    let passing_mode = if modifier.eq_ignore_ascii_case(b"ByVal") {
        Some(ModuleParameterPassingMode::ByVal)
    } else if modifier.eq_ignore_ascii_case(b"ByRef") {
        Some(ModuleParameterPassingMode::ByRef)
    } else {
        *cursor = start;
        None
    };
    if passing_mode.is_some() && !separator(text, cursor, check)? {
        return Ok(None);
    }
    let name_start = *cursor;
    let Some(spelling) = super::name(text, cursor, check)? else {
        return Ok(None);
    };
    if text.get(name_start) != Some(&b'[')
        && [
            b"ByVal".as_slice(),
            b"ByRef",
            b"Optional",
            b"ParamArray",
            b"As",
            b"Rem",
        ]
        .iter()
        .any(|keyword| spelling.eq_ignore_ascii_case(keyword))
    {
        return Ok(None);
    }
    let name = name_start..*cursor;
    if !separator(text, cursor, check)?
        || !word(text, cursor, check)?.eq_ignore_ascii_case(b"As")
        || !separator(text, cursor, check)?
    {
        return Ok(None);
    }
    let type_start = *cursor;
    if !super::module_body::intrinsic_type_name(word(text, cursor, check)?) {
        return Ok(None);
    }
    Ok(Some(ModuleParameter {
        declaration: start..*cursor,
        name,
        type_range: type_start..*cursor,
        passing_mode,
    }))
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
                assert_eq!(&source[parameter.type_range()], ty);
                assert_eq!(parameter.passing_mode(), passing_mode);
                assert!(procedure.header().start < parameter.declaration().start);
                assert!(parameter.declaration().end < procedure.header().end);
                assert!(parameter.declaration().start <= parameter.name().start);
                assert!(parameter.name().end < parameter.type_range().start);
                assert_eq!(parameter.type_range().end, parameter.declaration().end);
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
                    assert_eq!(&source[parameter.type_range()], ty);
                    assert_eq!(parameter.passing_mode(), mode);
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
            "value",
            "value As",
            "value As Long extra",
            "value As Long=1",
            "value As Long:=1",
            "value As Long,",
            ",value As Long",
            "value As Long,,other As Long",
            "value As Long,other",
            "value As Long other As Long",
            "value() As Long",
            "value(1) As Long",
            "value As Long()",
            "Optional value As Long",
            "ParamArray value() As Variant",
            "ByVal ByRef value As Long",
            "ByVal ByVal value As Long",
            "ByRef ByRef value As Long",
            "ByRef ByVal value As Long",
            "ByVal[value] As Long",
            "ByVal As Long",
            "ByRef As Long",
            "As As Long",
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
                    procedure.parameters()[0].type_range(),
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
                    ModuleBodyStatement::Declaration(_) | ModuleBodyStatement::Assignment(_) => {
                        None
                    }
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
            "Call Finish(1,target())",
            "Call Finish(1,target.member)",
            "Call Finish(1,(value))",
            "Call Finish(1,-2)",
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
            "Call Finish(1+2)",
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
            "Dim value As Long\nvalue = 1 + 2",
            "Const Limit As Long = other",
            "Dim x, y As Long",
            "Dim item As New Object",
            "Dim item As Product",
            "Dim values() As Long",
            "Dim value As Long=1",
            "value=1\nCall Finish(1+2)",
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
            "Dim value As Long\nSub Run()\nEnd Sub",
            "Const Limit As Long=1",
            "value=1",
        ] {
            assert!(matches!(
                parse(source.as_bytes()),
                Err(ModuleError::Unsupported { .. })
            ));
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
            "Sub Run(value)\nEnd Sub",
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
            "Dim value As Long",
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
}
