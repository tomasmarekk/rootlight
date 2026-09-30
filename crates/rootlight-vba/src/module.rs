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
}

impl ModuleLimits {
    /// Creates source-byte, logical-statement and retained-procedure ceilings.
    /// Zero ceilings are valid and admit only inputs requiring no such resource.
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
        }
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

/// One complete closed-slice Sub or Function retaining original source offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleProcedure {
    kind: ProcedureKind,
    declaration: Range<usize>,
    header: Range<usize>,
    name: Range<usize>,
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
    /// Fallible reservation of bounded procedure or body storage failed.
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
/// blank/comment segments and closed-slice Sub/Function procedures. Sub parameters
/// may be absent or explicitly empty; Function parameters must be explicitly
/// empty, with an optional intrinsic scalar As clause or unbracketed type suffix.
/// Public/Private and optional Static modifiers are accepted in that order.
/// Bodies admit one typed scalar Dim or Const per statement and simple scalar
/// assignments with an optional Let prefix. Const values are unsigned decimal,
/// physically closed doubled-quote strings or True/False; assignments additionally
/// admit a simple name. Keyword-shaped unbracketed names, calls and operators
/// are unsupported. Every body statement must be consumed completely.
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
                let Some(statement) = super::module_body::statement(source, range.clone(), check)
                    .map_err(ModuleError::Cancelled)?
                else {
                    return Err(ModuleError::Unsupported { range });
                };
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
        let Some((kind, name)) = empty_header(text, check).map_err(ModuleError::Cancelled)? else {
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
        open = Some(ModuleProcedure {
            kind,
            declaration: range.clone(),
            name: range.start + name.start..range.start + name.end,
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

fn empty_header<E>(
    text: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<(ProcedureKind, Range<usize>)>, E> {
    let mut prefix = 0;
    if word(text, &mut prefix, check)?.eq_ignore_ascii_case(b"Friend") {
        return Ok(None);
    }
    let Some(header) = procedure_header(text, check)? else {
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
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    let explicit_group = text.get(cursor) == Some(&b'(');
    if explicit_group {
        cursor += 1;
        cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
        if text.get(cursor) != Some(&b')') {
            return Ok(None);
        }
        cursor += 1;
    }
    if header.kind() == ProcedureKind::Sub {
        cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
        return Ok((cursor == text.len()).then(|| (header.kind(), header.name_span())));
    }
    if !explicit_group {
        return Ok(None);
    }
    let suffix = text
        .get(header.name_span().end.saturating_sub(1))
        .is_some_and(|byte| b"%&^!#@$".contains(byte));
    let separator = logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    cursor += separator;
    if cursor == text.len() {
        return Ok(Some((header.kind(), header.name_span())));
    }
    if suffix || separator == 0 {
        return Ok(None);
    }
    let Some(ty) = super::scalar_return_type(text, check)? else {
        return Ok(None);
    };
    let spelling = text.get(ty.clone()).unwrap_or_default();
    if !super::module_body::intrinsic_type_name(spelling) {
        return Ok(None);
    }
    cursor = ty.end;
    cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
    Ok((cursor == text.len()).then(|| (header.kind(), header.name_span())))
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
    fn scalar_body_unsupported_statement_rejects_the_whole_module_without_prefixes() {
        for body in [
            "Dim value As Long\nvalue = 1 + 2",
            "Const Limit As Long = other",
            "Dim x, y As Long",
            "Dim item As New Object",
            "Dim item As Product",
            "Dim values() As Long",
            "Dim value As Long=1",
            "value=1\nCall Finish()",
            "value=Me",
            "value=Nothing",
            "value=Null",
            "value=Empty",
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
            "Sub Run(value As Long)\nEnd Sub",
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
