//! Reads a source-owned procedure prefix independently of the structural parser.
//! Shared by project binding; logical continuations are not declaration boundaries.

use std::ops::Range;

use super::{logical_whitespace_len, name};

/// Written procedure category, before parameter, return-type or body validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcedureKind {
    /// A source-defined Sub procedure.
    Sub,
    /// A source-defined Function procedure.
    Function,
    /// A property value reader.
    PropertyGet,
    /// A property value writer.
    PropertyLet,
    /// A property object-reference writer.
    PropertySet,
    /// A DLL Sub declaration, not a source-defined callback target.
    ExternalSub,
    /// A DLL Function declaration, not a source-defined callback target.
    ExternalFunction,
}

/// A procedure prefix with a borrowed ASCII name and its original source range.
///
/// This establishes written category and spelling only. Parameters, return type,
/// body validity, accessibility and namespace uniqueness remain caller obligations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcedureHeader<'a> {
    kind: ProcedureKind,
    name: &'a [u8],
    start: usize,
    end: usize,
}

impl<'a> ProcedureHeader<'a> {
    /// Returns the category written in the declaration prefix.
    #[must_use]
    pub const fn kind(self) -> ProcedureKind {
        self.kind
    }

    /// Returns the name without brackets or a type suffix, preserving letter case.
    #[must_use]
    pub const fn name(self) -> &'a [u8] {
        self.name
    }

    /// Returns the exact name range, including authored brackets and type suffix.
    #[must_use]
    pub fn name_span(self) -> Range<usize> {
        self.start..self.end
    }
}

/// Reads a VBA procedure prefix through its name without a parser runtime.
///
/// Input starts at a declaration, with optional leading logical whitespace.
/// Public, Private, Friend, Static, Declare and PtrSafe prefixes are recognized
/// case-insensitively. ASCII names may be bracketed; function/getter names may
/// have a type suffix. Logical continuations may separate tokens; an ordinary
/// newline, comment or statement separator cannot join separate declarations.
/// Unsupported or malformed prefixes return None, never a partial category.
/// This does not validate the parameter list, return type or procedure body.
/// No allocation occurs. The callback runs before parsing and during long scans.
///
/// # Errors
/// Returns the caller's error unchanged when its cancellation/work callback fails.
///
/// # Examples
/// ```
/// use rootlight_vba::{ProcedureKind, procedure_header};
/// let source = b"Public Static Property _\r\nGet [Value]() As Long";
/// let header = procedure_header(source, &mut || Ok::<(), std::convert::Infallible>(()))?
///     .expect("the literal has a procedure prefix");
/// assert_eq!(header.kind(), ProcedureKind::PropertyGet);
/// assert_eq!(header.name(), b"Value");
/// assert_eq!(&source[header.name_span()], b"[Value]");
/// # Ok::<(), std::convert::Infallible>(())
/// ```
pub fn procedure_header<'a, E>(
    source: &'a [u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ProcedureHeader<'a>>, E> {
    check()?;
    let mut cursor = 0;
    let mut word = next_word(source, &mut cursor, check)?;
    let friend = word.eq_ignore_ascii_case(b"Friend");
    if friend || word.eq_ignore_ascii_case(b"Public") || word.eq_ignore_ascii_case(b"Private") {
        word = next_word(source, &mut cursor, check)?;
    }
    let static_storage = word.eq_ignore_ascii_case(b"Static");
    if static_storage {
        word = next_word(source, &mut cursor, check)?;
    }
    let external = word.eq_ignore_ascii_case(b"Declare");
    if external {
        if friend || static_storage {
            return Ok(None);
        }
        word = next_word(source, &mut cursor, check)?;
        if word.eq_ignore_ascii_case(b"PtrSafe") {
            word = next_word(source, &mut cursor, check)?;
        }
    }
    let kind = if word.eq_ignore_ascii_case(b"Sub") {
        if external {
            ProcedureKind::ExternalSub
        } else {
            ProcedureKind::Sub
        }
    } else if word.eq_ignore_ascii_case(b"Function") {
        if external {
            ProcedureKind::ExternalFunction
        } else {
            ProcedureKind::Function
        }
    } else if !external && word.eq_ignore_ascii_case(b"Property") {
        let accessor = next_word(source, &mut cursor, check)?;
        if accessor.eq_ignore_ascii_case(b"Get") {
            ProcedureKind::PropertyGet
        } else if accessor.eq_ignore_ascii_case(b"Let") {
            ProcedureKind::PropertyLet
        } else if accessor.eq_ignore_ascii_case(b"Set") {
            ProcedureKind::PropertySet
        } else {
            return Ok(None);
        }
    } else {
        return Ok(None);
    };
    let separator = logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    if separator == 0 {
        return Ok(None);
    }
    cursor += separator;
    let start = cursor;
    let Some(name) = name(source, &mut cursor, check)? else {
        return Ok(None);
    };
    if matches!(
        source.get(cursor),
        Some(b'%' | b'&' | b'^' | b'!' | b'#' | b'@' | b'$')
    ) {
        if !matches!(
            kind,
            ProcedureKind::Function | ProcedureKind::PropertyGet | ProcedureKind::ExternalFunction
        ) || source.get(start) == Some(&b'[')
        {
            return Ok(None);
        }
        cursor += 1;
    }
    if !matches!(
        source.get(cursor),
        None | Some(b'(' | b' ' | b'\t' | b'\r' | b'\n' | b':' | b'\'')
    ) {
        return Ok(None);
    }
    Ok(Some(ProcedureHeader {
        kind,
        name,
        start,
        end: cursor,
    }))
}

/// Returns the exact scalar return-type range in a Function or Property Get header.
///
/// Requires a complete parameter group and an explicit As clause. Arrays, type
/// suffixes, construction modifiers and malformed clauses return None. The range
/// is relative to the supplied declaration, preserving qualified names. This is
/// spelling evidence only; the caller proves the type's identity and visibility.
///
/// # Errors
/// Propagates the caller's cancellation/work error unchanged.
pub fn scalar_return_type<E>(
    source: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Range<usize>>, E> {
    let Some(header) = procedure_header(source, check)? else {
        return Ok(None);
    };
    if !matches!(
        header.kind(),
        ProcedureKind::Function | ProcedureKind::PropertyGet
    ) || source
        .get(header.name_span().end.saturating_sub(1))
        .is_some_and(|byte| b"%&^!#@$".contains(byte))
    {
        return Ok(None);
    }
    let mut cursor = header.name_span().end;
    cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    let Some(end) =
        super::parenthesized_expression_end(source.get(cursor..).unwrap_or_default(), check)?
    else {
        return Ok(None);
    };
    cursor += end;
    if logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)? == 0 {
        return Ok(None);
    }
    if !next_word(source, &mut cursor, check)?.eq_ignore_ascii_case(b"As") {
        return Ok(None);
    }
    let separator = logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    if separator == 0 {
        return Ok(None);
    }
    cursor += separator;
    let Some(ty) = super::procedure_pointer_path(source.get(cursor..).unwrap_or_default(), check)?
    else {
        return Ok(None);
    };
    Ok(Some(cursor..cursor + ty.end()))
}

fn next_word<'a, E>(
    source: &'a [u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<&'a [u8], E> {
    *cursor += logical_whitespace_len(source.get(*cursor..).unwrap_or_default(), check)?;
    let start = *cursor;
    while let Some(byte) = source.get(*cursor) {
        if cursor.is_multiple_of(512) {
            check()?;
        }
        if !byte.is_ascii_alphanumeric() && *byte != b'_' {
            break;
        }
        *cursor += 1;
    }
    Ok(source.get(start..*cursor).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_return_ranges_exclude_arrays_and_body_type_mentions() {
        for (source, expected) in [
            (
                "Public Function Build(value As Long) As Product\nEnd Function",
                Some("Product"),
            ),
            (
                "Function Build() _\r\n As _\n [Module].[Product] ' note\nEnd Function",
                Some("[Module].[Product]"),
            ),
            (
                "Property Get Item() As Product: End Property",
                Some("Product"),
            ),
            ("Function Build() As Product()\nEnd Function", None),
            ("Function Build()\nDim item As Product\nEnd Function", None),
            ("Function Build() As New Product\nEnd Function", None),
            ("Function Build$() As Product\nEnd Function", None),
            ("Sub Build() As Product\nEnd Sub", None),
            ("Function Build( As Product", None),
        ] {
            let range = scalar_return_type(source.as_bytes(), &mut || Ok::<(), ()>(())).unwrap();
            assert_eq!(
                range.and_then(|range| source.get(range)),
                expected,
                "{source}"
            );
        }
        let source = b"Function Build() As Product";
        assert_eq!(
            scalar_return_type(source, &mut || Err("cancelled")),
            Err("cancelled")
        );
        for end in 0..=source.len() {
            let input = source.get(..end).unwrap();
            if let Some(range) = scalar_return_type(input, &mut || Ok::<(), ()>(())).unwrap() {
                assert!(range.start < range.end && range.end <= input.len());
            }
        }
    }

    #[test]
    fn prefixes_preserve_category_name_and_source_boundaries() {
        for (source, kind, spelling, name) in [
            ("Sub Notify()", ProcedureKind::Sub, "Notify", "Notify"),
            (
                "Private Function Value$()",
                ProcedureKind::Function,
                "Value$",
                "Value",
            ),
            (
                "pUbLiC _\r\nStatic _\nSub [Notify]()",
                ProcedureKind::Sub,
                "[Notify]",
                "Notify",
            ),
            (
                "Friend Static Property _\nGet Value%()",
                ProcedureKind::PropertyGet,
                "Value%",
                "Value",
            ),
            (
                "Static Property Let Value(ByVal nextValue As Long)",
                ProcedureKind::PropertyLet,
                "Value",
                "Value",
            ),
            (
                "Property Set _\nValue(ByVal nextValue As Object)",
                ProcedureKind::PropertySet,
                "Value",
                "Value",
            ),
            (
                "Public Declare _\nPtrSafe _\nFunction Clock Lib \"clock\" ()",
                ProcedureKind::ExternalFunction,
                "Clock",
                "Clock",
            ),
            (
                "Private Declare Sub Register Lib \"host\" ()",
                ProcedureKind::ExternalSub,
                "Register",
                "Register",
            ),
            (
                " \tSub Notify\nEnd Sub",
                ProcedureKind::Sub,
                "Notify",
                "Notify",
            ),
        ] {
            let header = procedure_header(source.as_bytes(), &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap();
            assert_eq!(header.kind(), kind, "{source}");
            assert_eq!(header.name(), name.as_bytes(), "{source}");
            assert_eq!(source.get(header.name_span()), Some(spelling), "{source}");
        }
    }

    #[test]
    fn malformed_or_separate_prefixes_never_supply_a_category() {
        for source in [
            "",
            "Public",
            "Sub",
            "Submarine Notify()",
            "Public Private Sub Notify()",
            "Static Public Sub Notify()",
            "Declare Property Get Value()",
            "Friend Declare Sub Notify()",
            "Static Declare Sub Notify()",
            "Declare PtrSafe PtrSafe Function Value()",
            "Public\nSub Notify()",
            "Public ' comment\nSub Notify()",
            "Public:Sub Notify()",
            "Public _ x\nSub Notify()",
            "Property Getter Value()",
            "Property Get\nValue()",
            "Sub[Notify]()",
            "Sub Notify$()",
            "Property Let Value%()",
            "Function [Value]$()",
            "Sub Notify.member()",
            "Sub Notify!()",
            "Sub [Notify",
            "Sub \"Notify\"()",
            "Public \0 Sub Notify()",
            "Sub Nótify()",
        ] {
            assert_eq!(
                procedure_header(source.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                None,
                "{source:?}"
            );
        }
    }

    #[test]
    fn scans_are_cancellable_and_never_slice_outside_arbitrary_bytes() {
        for input in [
            vec![b' '; 4096],
            vec![b'A'; 4096],
            [b"Sub ".as_slice(), vec![b'A'; 4096].as_slice()].concat(),
            [
                b"Public ".as_slice(),
                b" _\n".repeat(1024).as_slice(),
                b"Sub Notify()",
            ]
            .concat(),
        ] {
            let mut calls = 0;
            assert_eq!(
                procedure_header(&input, &mut || {
                    calls += 1;
                    if calls == 5 { Err("cancelled") } else { Ok(()) }
                }),
                Err("cancelled")
            );
            assert_eq!(calls, 5);
        }
        for byte in 0..=u8::MAX {
            for input in [
                vec![byte],
                [b"Sub A".as_slice(), &[byte]].concat(),
                [b"Property ".as_slice(), &[byte], b" Value()"].concat(),
            ] {
                if let Some(header) = procedure_header(&input, &mut || Ok::<(), ()>(())).unwrap() {
                    assert!(input.get(header.name_span()).is_some());
                    assert!(!header.name().is_empty());
                }
            }
        }
    }
}
