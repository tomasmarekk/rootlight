//! Reads source-written VBA names, declaration prefixes and expression boundaries.
//! No parser runtime is required. Identifier paths retain argument boundaries;
//! procedure categories come from source, never from an uncontinued following line.

#![forbid(unsafe_code)]

mod arguments;
mod compilation;
mod group;
mod parameter;
mod procedure;

pub use arguments::{Argument, argument_list};
pub use compilation::{CompilationConstantHeader, compilation_constant_header};
pub use group::parenthesized_expression_end;
pub use parameter::array_parameter_modifiers;
pub use procedure::{ProcedureHeader, ProcedureKind, procedure_header};

/// A syntactically valid pointer path borrowing its original source bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcedurePath<'a> {
    end: usize,
    receiver_end: Option<usize>,
    components: usize,
    first: &'a [u8],
    last: &'a [u8],
}

impl<'a> ProcedurePath<'a> {
    /// Exclusive byte offset of the last identifier, relative to the supplied input.
    #[must_use]
    pub const fn end(self) -> usize {
        self.end
    }

    /// Exclusive end of the receiver before the final dot, or None for a simple name.
    ///
    /// The offset is relative to the supplied input and excludes trivia before
    /// the final dot. Earlier continuations and authored brackets remain inside
    /// the receiver range, allowing exact source-identity lookup without rewriting.
    #[must_use]
    pub const fn receiver_end(self) -> Option<usize> {
        self.receiver_end
    }

    /// Number of identifiers, including the final target name.
    #[must_use]
    pub const fn components(self) -> usize {
        self.components
    }

    /// First identifier without brackets, preserving authored letter case.
    #[must_use]
    pub const fn first(self) -> &'a [u8] {
        self.first
    }

    /// Last identifier without brackets, preserving authored letter case.
    #[must_use]
    pub const fn last(self) -> &'a [u8] {
        self.last
    }
}

/// Reads one VBA procedure-pointer name without allocating or resolving a symbol.
///
/// Input starts at the first identifier. ASCII names may be bracketed and separated
/// by dots with legal logical-line continuations. The returned end excludes trailing
/// trivia and argument delimiters. Unsupported or malformed syntax returns None.
/// This validates spelling only; callers establish namespace, access and project identity.
/// The callback runs before parsing and periodically during every byte scan.
///
/// # Errors
/// Returns the caller's error unchanged if the cancellation/work callback fails.
///
/// # Examples
/// ```
/// use rootlight_vba::procedure_pointer_path;
/// let source = b"[Callbacks].Notify, next";
/// let path = procedure_pointer_path(source, &mut || Ok::<(), std::convert::Infallible>(()))?
///     .expect("the literal contains a complete pointer path");
/// assert_eq!(path.first(), b"Callbacks");
/// assert_eq!(path.last(), b"Notify");
/// assert_eq!(&source[..path.end()], b"[Callbacks].Notify");
/// # Ok::<(), std::convert::Infallible>(())
/// ```
pub fn procedure_pointer_path<'a, E>(
    source: &'a [u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<ProcedurePath<'a>>, E> {
    check()?;
    let mut cursor = 0;
    let Some(first) = name(source, &mut cursor, check)? else {
        return Ok(None);
    };
    let mut result = ProcedurePath {
        end: cursor,
        receiver_end: None,
        components: 1,
        first,
        last: first,
    };
    loop {
        let before_trivia = cursor;
        let continued = trivia(source, &mut cursor, check)?;
        match source.get(cursor) {
            Some(b'.') => {
                // MS-VBAL 5.6.12 requires adjacency or an explicit continuation
                // before the member operator, not arbitrary separating spaces.
                if cursor != before_trivia && !continued {
                    return Ok(None);
                }
                cursor += 1;
                trivia(source, &mut cursor, check)?;
                let Some(last) = name(source, &mut cursor, check)? else {
                    return Ok(None);
                };
                result.receiver_end = Some(result.end);
                result.end = cursor;
                // Each component consumes at least one byte of the input.
                result.components += 1;
                result.last = last;
            }
            None | Some(b',' | b')' | b'\r' | b'\n' | b':' | b'\'') => return Ok(Some(result)),
            _ => return Ok(None),
        }
    }
}

fn name<'a, E>(
    source: &'a [u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<&'a [u8]>, E> {
    let quoted = source.get(*cursor) == Some(&b'[');
    if quoted {
        *cursor += 1;
    }
    let start = *cursor;
    if !source
        .get(*cursor)
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    {
        return Ok(None);
    }
    while let Some(byte) = source.get(*cursor) {
        if cursor.is_multiple_of(512) {
            check()?;
        }
        if !byte.is_ascii_alphanumeric() && *byte != b'_' {
            break;
        }
        *cursor += 1;
    }
    let name = source.get(start..*cursor);
    if quoted {
        if source.get(*cursor) != Some(&b']') {
            return Ok(None);
        }
        *cursor += 1;
    }
    Ok(name)
}

/// Returns the byte length of leading spaces, tabs and explicit line continuations.
///
/// A continuation requires separating whitespace, an underscore and an LF or CRLF
/// line ending. Bare line endings, comments and other tokens are not consumed.
/// Empty input returns zero. No allocation or character decoding is performed.
/// The callback runs before scanning and periodically during long whitespace runs.
///
/// # Errors
/// Returns the caller's error unchanged when its cancellation/work callback fails.
pub fn logical_whitespace_len<E>(
    source: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<usize, E> {
    let mut cursor = 0;
    trivia(source, &mut cursor, check)?;
    Ok(cursor)
}

fn trivia<E>(
    source: &[u8],
    cursor: &mut usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let mut continued = false;
    loop {
        check()?;
        let start = *cursor;
        while matches!(source.get(*cursor), Some(b' ' | b'\t')) {
            if cursor.is_multiple_of(512) {
                check()?;
            }
            *cursor += 1;
        }
        // A continuation needs separating whitespace. Ordinary newlines end
        // the argument and must not consume a following With-member statement.
        if *cursor == start || source.get(*cursor) != Some(&b'_') {
            return Ok(continued);
        }
        let mut end = *cursor + 1;
        while matches!(source.get(end), Some(b' ' | b'\t')) {
            if end.is_multiple_of(512) {
                check()?;
            }
            end += 1;
        }
        if source.get(end) == Some(&b'\r') {
            end += 1;
        }
        if source.get(end) != Some(&b'\n') {
            return Ok(continued);
        }
        *cursor = end + 1;
        continued = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_receivers_exclude_only_final_separator_trivia() {
        for (source, receiver, last) in [
            ("Name", None, "Name"),
            ("[Owner].[Value]", Some("[Owner]"), "Value"),
            ("item _\n.Child", Some("item"), "Child"),
            ("item. _\r\n Child", Some("item"), "Child"),
            (
                "item _\n.[Child] _\r\n. _\n Value",
                Some("item _\n.[Child]"),
                "Value",
            ),
            ("item.Child, next", Some("item"), "Child"),
        ] {
            let path = procedure_pointer_path(source.as_bytes(), &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap();
            assert_eq!(
                path.receiver_end().and_then(|end| source.get(..end)),
                receiver,
                "{source}"
            );
            assert_eq!(path.last(), last.as_bytes());
        }
    }

    #[test]
    fn logical_whitespace_preserves_tokens_and_physical_line_boundaries() {
        for (source, expected) in [
            ("", ""),
            (" \tlabel", " \t"),
            (" _\r\n \t_ \n :", " _\r\n \t_ \n "),
            ("\nlabel", ""),
            (" \r\nlabel", " "),
            ("_\nlabel", ""),
            (" _ :", " "),
            (" _\rlabel", " "),
            (" 'comment", " "),
        ] {
            let length =
                logical_whitespace_len(source.as_bytes(), &mut || Ok::<(), ()>(())).unwrap();
            assert_eq!(source.get(..length), Some(expected), "{source:?}");
        }
        let long = vec![b' '; 4096];
        for input in [long, b" _\n".repeat(1024)] {
            let mut calls = 0;
            assert_eq!(
                logical_whitespace_len(&input, &mut || {
                    calls += 1;
                    if calls == 3 { Err("cancelled") } else { Ok(()) }
                }),
                Err("cancelled")
            );
        }
    }

    #[test]
    fn paths_keep_boundaries_quoting_and_logical_line_continuations() {
        for (source, target, first, last, components) in [
            (
                "Module.Callback, other",
                "Module.Callback",
                "Module",
                "Callback",
                2,
            ),
            (
                "[Module].[Callback])",
                "[Module].[Callback]",
                "Module",
                "Callback",
                2,
            ),
            (
                "Module _\r\n.Callback",
                "Module _\r\n.Callback",
                "Module",
                "Callback",
                2,
            ),
            (
                "Module. _\nCallback",
                "Module. _\nCallback",
                "Module",
                "Callback",
                2,
            ),
            (
                "Project.Module.Callback' note",
                "Project.Module.Callback",
                "Project",
                "Callback",
                3,
            ),
            ("Callback\n.Member", "Callback", "Callback", "Callback", 1),
        ] {
            let path = procedure_pointer_path(source.as_bytes(), &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap();
            assert_eq!(source.get(..path.end), Some(target));
            assert_eq!(path.first, first.as_bytes());
            assert_eq!(path.last, last.as_bytes());
            assert_eq!(path.components, components);
        }
    }

    #[test]
    fn invalid_names_never_return_a_valid_prefix() {
        for source in [
            "",
            ".Callback",
            "Module.",
            "Module..Callback",
            "Module .Callback",
            "Module!Callback",
            "Module.Callback$",
            "Module.Callback(1)",
            "[Module.Callback",
            "Module.Callback + 1",
            "Module.[Callback ]",
            "Module.\nCallback",
            "Module _\n.Callback(",
        ] {
            assert!(
                procedure_pointer_path(source.as_bytes(), &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn paths_observe_cancellation_and_arbitrary_byte_boundaries() {
        assert_eq!(
            procedure_pointer_path(b"Module.Callback", &mut || Err("cancelled")),
            Err("cancelled")
        );
        for input in [
            vec![b'A'; 4096],
            [
                b"Module.".as_slice(),
                vec![b' '; 4096].as_slice(),
                b"Callback",
            ]
            .concat(),
        ] {
            for maximum in [1, 3, 6] {
                let mut calls = 0;
                let result = procedure_pointer_path(&input, &mut || {
                    calls += 1;
                    if calls == maximum {
                        Err("budget")
                    } else {
                        Ok(())
                    }
                });
                assert_eq!(result, Err("budget"));
                assert_eq!(calls, maximum);
            }
        }
        for byte in 0..=u8::MAX {
            for input in [vec![byte], vec![b'A', b'.', byte], vec![b'[', byte, b']']] {
                if let Some(path) =
                    procedure_pointer_path(&input, &mut || Ok::<(), ()>(())).unwrap()
                {
                    assert!(path.end <= input.len());
                    assert!(path.components <= path.end);
                    assert!(!path.first.is_empty() && !path.last.is_empty());
                }
            }
        }
    }
}
