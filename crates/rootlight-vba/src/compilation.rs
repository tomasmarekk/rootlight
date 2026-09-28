//! Reads compilation-constant headers without evaluating their expressions.
//! Compiler constants cannot change runtime declaration membership without a
//! separate conditional directive; callers must still inspect those directives.

use std::ops::Range;

use super::{logical_whitespace_len, name};

/// A source-written compiler constant's name and initializer boundary.
///
/// This proves only the `#Const name =` prefix, not expression validity,
/// uniqueness, a constant value, or inclusion of surrounding conditional code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompilationConstantHeader<'a> {
    name: &'a [u8],
    start: usize,
    end: usize,
    initializer: usize,
}

impl<'a> CompilationConstantHeader<'a> {
    /// Returns the ASCII name without brackets or type suffix, preserving letter case.
    #[must_use]
    pub const fn name(self) -> &'a [u8] {
        self.name
    }

    /// Returns the exact name range, including authored brackets or type suffix.
    #[must_use]
    pub fn name_span(self) -> Range<usize> {
        self.start..self.end
    }

    /// Returns the byte offset immediately after `=`, before initializer trivia.
    #[must_use]
    pub const fn initializer_start(self) -> usize {
        self.initializer
    }
}

/// Reads a VBA compilation-constant directive through its assignment marker.
///
/// Input must start at a logical line boundary. Leading spaces, tabs and logical
/// continuations are accepted. `Const` is case-insensitive; ASCII names may be
/// bracketed or have a type suffix. A bare newline, comment,
/// other directive or malformed header returns None. Initializer bytes are not
/// examined, so even a valid prefix does not prove a valid constant expression.
/// No allocation occurs; the callback runs before parsing and during long scans.
///
/// # Errors
/// Returns the caller's error unchanged when its cancellation/work callback fails.
///
/// # Examples
/// ```
/// use rootlight_vba::compilation_constant_header;
/// let input = b"#Const Feature% = True";
/// let header = compilation_constant_header(input, &mut || Ok::<(), std::convert::Infallible>(()))?
///     .expect("the literal contains a constant header");
/// assert_eq!(header.name(), b"Feature");
/// assert_eq!(&input[header.name_span()], b"Feature%");
/// assert_eq!(&input[header.initializer_start()..], b" True");
/// # Ok::<(), std::convert::Infallible>(())
/// ```
pub fn compilation_constant_header<'a, E>(
    source: &'a [u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<CompilationConstantHeader<'a>>, E> {
    let mut cursor = logical_whitespace_len(source, check)?;
    if source.get(cursor) != Some(&b'#') {
        return Ok(None);
    }
    cursor += 1;
    cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    let Some(rest) = source.get(cursor..) else {
        return Ok(None);
    };
    if !rest
        .get(..5)
        .is_some_and(|word| word.eq_ignore_ascii_case(b"Const"))
    {
        return Ok(None);
    }
    cursor += 5;
    let separator = logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    if separator == 0 {
        return Ok(None);
    }
    cursor += separator;
    let start = cursor;
    let Some(name) = name(source, &mut cursor, check)? else {
        return Ok(None);
    };
    // A compiler constant's written type suffix does not affect its Variant
    // binding type or name value (MS-VBAL 3.4.1).
    if source.get(start) != Some(&b'[')
        && matches!(
            source.get(cursor),
            Some(b'%' | b'&' | b'^' | b'!' | b'#' | b'@' | b'$')
        )
    {
        cursor += 1;
    }
    let end = cursor;
    cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    if source.get(cursor) != Some(&b'=') {
        return Ok(None);
    }
    Ok(Some(CompilationConstantHeader {
        name,
        start,
        end,
        initializer: cursor + 1,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_headers_preserve_names_and_initializer_boundaries() {
        for (source, expected, spelling) in [
            ("#Const Feature = True", "Feature", "Feature"),
            (" \t#cOnSt Feature_2=other + 1", "Feature_2", "Feature_2"),
            ("# Const Feature =", "Feature", "Feature"),
            (
                "# _\r\nConst _\nFeature _\r\n= expression",
                "Feature",
                "Feature",
            ),
            ("#Const [Feature] = True", "Feature", "[Feature]"),
            ("#Const Feature$ = True", "Feature", "Feature$"),
        ] {
            let header = compilation_constant_header(source.as_bytes(), &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap();
            assert_eq!(header.name(), expected.as_bytes());
            assert_eq!(source.get(header.name_span()), Some(spelling));
            assert_eq!(
                source.as_bytes().get(header.initializer_start() - 1),
                Some(&b'=')
            );
        }
    }

    #[test]
    fn other_directives_and_malformed_headers_are_not_constants() {
        for source in [
            "",
            "Const Feature = True",
            "#If Feature Then",
            "#ElseIf Feature Then",
            "#Else",
            "#End If",
            "#Unknown",
            "#Constant Feature = True",
            "#ConstFeature = True",
            "#Const",
            "#Const Feature",
            "#Const [Feature]$ = True",
            "#Const Feature$$ = True",
            "#Const Feature.Member = True",
            "#Const 1 = True",
            "#Const\nFeature = True",
            "#Const Feature\n= True",
            "#Const Feature ' = True",
            "' #Const Feature = True",
            "#Const Feature: = True",
        ] {
            assert!(
                compilation_constant_header(source.as_bytes(), &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn header_scans_cancel_and_preserve_arbitrary_byte_bounds() {
        for input in [
            vec![b' '; 8192],
            format!("#Const {} = 0", "A".repeat(8192)).into_bytes(),
        ] {
            for limit in [1, 3, 6] {
                let mut calls = 0;
                assert_eq!(
                    compilation_constant_header(&input, &mut || {
                        calls += 1;
                        if calls == limit {
                            Err("cancelled")
                        } else {
                            Ok(())
                        }
                    }),
                    Err("cancelled")
                );
                assert_eq!(calls, limit);
            }
        }
        for byte in 0..=u8::MAX {
            let input = [b"#Const ".as_slice(), &[byte], b" = 0"].concat();
            for length in 0..=input.len() {
                if let Some(header) =
                    compilation_constant_header(&input[..length], &mut || Ok::<(), ()>(())).unwrap()
                {
                    assert!(header.name_span().end <= header.initializer_start());
                    assert!(header.initializer_start() <= length);
                    assert_eq!(header.name(), &input[header.name_span()]);
                }
            }
        }
    }
}
