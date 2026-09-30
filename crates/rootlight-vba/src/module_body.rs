//! Complete scalar-local statements for the closed first-party module inventory.
//! Records own only original byte ranges; declaration identity and reference roles
//! remain obligations of the adapter's existing structural normalization boundary.

use std::ops::Range;

use super::{logical_whitespace_len, module::ModuleError, name};

/// A completely admitted local statement inside one procedural body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleBodyStatement {
    /// One explicitly typed scalar Dim or Const declaration.
    Declaration(ModuleLocalDeclaration),
    /// One simple scalar assignment, optionally introduced by Let.
    Assignment(ModuleScalarAssignment),
}

impl ModuleBodyStatement {
    /// Returns the exact trimmed logical statement, excluding trailing comments.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        match self {
            Self::Declaration(declaration) => declaration.statement(),
            Self::Assignment(assignment) => assignment.statement(),
        }
    }
}

/// The written category of an explicitly typed scalar local declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleLocalKind {
    /// A local Dim variable without an initializer.
    Variable,
    /// A local Const with an atomic literal initializer.
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

    /// Returns the atomic literal initializer for Const, or None for Dim.
    #[must_use]
    pub fn value(&self) -> Option<&ModuleScalarValue> {
        self.value.as_ref()
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

/// Syntax categories admitted as complete atomic scalar values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleScalarValueKind {
    /// One or more decimal digits without sign, separator, exponent or suffix.
    UnsignedDecimal,
    /// One physically closed string, with optional doubled-quote escapes.
    String,
    /// An unbracketed True or False keyword, without coercion or evaluation.
    Boolean,
    /// One ASCII name, retaining authored brackets and requiring later binding.
    Name,
}

/// One complete atomic value retaining its original source spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleScalarValue {
    kind: ModuleScalarValueKind,
    span: Range<usize>,
}

impl ModuleScalarValue {
    /// Returns the value's syntax category, not its runtime type or target identity.
    #[must_use]
    pub const fn kind(&self) -> ModuleScalarValueKind {
        self.kind
    }

    /// Returns the exact value token; strings include both authored quotes.
    #[must_use]
    pub fn span(&self) -> Range<usize> {
        self.span.clone()
    }
}

pub(super) fn statement<E>(
    source: &[u8],
    range: Range<usize>,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleBodyStatement>, E> {
    check()?;
    let text = source.get(range.clone()).unwrap_or_default();
    let mut cursor = 0;
    let first = word(text, &mut cursor, check)?;
    let local = if first.eq_ignore_ascii_case(b"Dim") {
        Some(ModuleLocalKind::Variable)
    } else if first.eq_ignore_ascii_case(b"Const") {
        Some(ModuleLocalKind::Constant)
    } else {
        None
    };
    if let Some(kind) = local {
        return local_declaration(text, range, cursor, kind, check);
    }
    if first.eq_ignore_ascii_case(b"Let") {
        if !separator(text, &mut cursor, check)? {
            return Ok(None);
        }
    } else {
        cursor = 0;
    }
    let Some(destination) = simple_name(text, &mut cursor, check)? else {
        return Ok(None);
    };
    if !equals(text, &mut cursor, check)? {
        return Ok(None);
    }
    let Some(value) = atomic_value(text, &mut cursor, true, check)? else {
        return Ok(None);
    };
    if !complete(text, &mut cursor, check)? {
        return Ok(None);
    }
    Ok(Some(ModuleBodyStatement::Assignment(
        ModuleScalarAssignment {
            destination: absolute(&range, destination),
            value: ModuleScalarValue {
                kind: value.kind,
                span: absolute(&range, value.span),
            },
            statement: range,
        },
    )))
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
        let Some(value) = atomic_value(text, &mut cursor, false, check)? else {
            return Ok(None);
        };
        Some(ModuleScalarValue {
            kind: value.kind,
            span: absolute(&range, value.span),
        })
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

fn atomic_value<E>(
    text: &[u8],
    cursor: &mut usize,
    allow_name: bool,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ModuleScalarValue>, E> {
    let start = *cursor;
    let kind = if text.get(*cursor).is_some_and(u8::is_ascii_digit) {
        while text.get(*cursor).is_some_and(u8::is_ascii_digit) {
            if cursor.is_multiple_of(512) {
                check()?;
            }
            *cursor += 1;
        }
        ModuleScalarValueKind::UnsignedDecimal
    } else if text.get(*cursor) == Some(&b'"') {
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
        span: start..*cursor,
    }))
}

fn simple_name<E>(
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
        statement(text, 0..text.len(), &mut || Ok::<(), ()>(())).unwrap()
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
    fn unsupported_local_forms_and_value_tails_never_return_a_prefix() {
        for source in [
            "Dim count",
            "Dim count As",
            "Dim count As Product",
            "Dim count As [Long]",
            "Dim count() As Long",
            "Dim count As Long()",
            "Dim count As New Object",
            "Dim count As String * 8",
            "Dim count As Long = 1",
            "Dim x, y As Long",
            "Dim count$ As String",
            "Dim Me As Long",
            "Dim Dim As Long",
            "Static count As Long",
            "Private Dim count As Long",
            "Const Limit As Long",
            "Const Limit As Long = other",
            "Const Limit As Long = 1, Other = 2",
            "Const Limit As Long = -1",
            "count = -1",
            "count = +1",
            "count = 1.0",
            "count = 1E2",
            "count = &H10",
            "count = 1&",
            "count = 1more",
            "count = \"closed\" extra",
            "count = \"open",
            "count = \"continued _\n",
            "count = \"physical\nline\"",
            "count = (1)",
            "count = 1 + 2",
            "count = target()",
            "count = target.member",
            "count = Me",
            "count = Nothing",
            "count = Empty",
            "count = Null",
            "count = Not",
            "Me = count",
            "Rem = count",
            "count$ = 1",
            "count(0) = 1",
            "item.member = count",
            "Set item = other",
            "Call Finish()",
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
    fn offsets_are_module_relative_and_all_arbitrary_suffixes_remain_bounded() {
        let source = b"prefix\nDim count As Long\ntail";
        let ModuleBodyStatement::Declaration(declaration) =
            statement(source, 7..24, &mut || Ok::<(), ()>(()))
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
        let mut statements = Vec::new();
        assert_eq!(
            reserve::<()>(&mut statements, 0),
            Err(ModuleError::StatementLimit)
        );
        reserve::<()>(&mut statements, 1).unwrap();
        statements.push(read(b"count=1").unwrap());
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
                    statement(source.as_bytes(), 0..source.len(), &mut || {
                        calls += 1;
                        if calls == limit { Err("stop") } else { Ok(()) }
                    }),
                    Err("stop")
                );
                assert_eq!(calls, limit);
            }
        }
    }
}
