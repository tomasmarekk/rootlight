//! Validates authored ParamArray declarations and positional argument shape.
//! Both native and project binding use this first-party contract; expression
//! typing and proof of the surrounding procedure remain caller responsibilities.

use super::{Argument, logical_whitespace_len, name};

/// Reads the name from one complete, valid ParamArray parameter declaration.
///
/// The declaration must contain an empty array designator and may explicitly
/// specify only Variant (also bracketed). No other modifier, type suffix, array
/// bound or default is accepted. ASCII case and logical continuations are allowed.
/// Returns None for ordinary parameters or malformed declarations. The caller
/// must prove this is the last parameter and no preceding parameter is optional.
/// Parsing borrows the source without allocation or recursion.
///
/// # Errors
/// Returns the caller's cancellation/work error unchanged.
pub fn param_array_parameter<'a, E>(
    source: &'a [u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<&'a [u8]>, E> {
    let mut cursor = logical_whitespace_len(source, check)?;
    let keyword_start = cursor;
    let Some(keyword) = name(source, &mut cursor, check)? else {
        return Ok(None);
    };
    if !keyword.eq_ignore_ascii_case(b"ParamArray") || source.get(keyword_start) == Some(&b'[') {
        return Ok(None);
    }
    let whitespace = logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    if whitespace == 0 {
        return Ok(None);
    }
    cursor += whitespace;
    let Some(parameter) = name(source, &mut cursor, check)? else {
        return Ok(None);
    };
    cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    if source.get(cursor) != Some(&b'(') {
        return Ok(None);
    }
    cursor += 1;
    cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    if source.get(cursor) != Some(&b')') {
        return Ok(None);
    }
    cursor += 1;
    let whitespace = logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    cursor += whitespace;
    if cursor == source.len() {
        return Ok(Some(parameter));
    }
    if whitespace == 0 || source.get(cursor) == Some(&b'[') {
        return Ok(None);
    }
    if !name(source, &mut cursor, check)?.is_some_and(|word| word.eq_ignore_ascii_case(b"As")) {
        return Ok(None);
    }
    let whitespace = logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    if whitespace == 0 {
        return Ok(None);
    }
    cursor += whitespace;
    if !name(source, &mut cursor, check)?.is_some_and(|word| word.eq_ignore_ascii_case(b"Variant"))
    {
        return Ok(None);
    }
    cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    Ok((cursor == source.len()).then_some(parameter))
}

/// Checks positional argument shape for required parameters followed by ParamArray.
///
/// `required` excludes the final ParamArray parameter. Its elements may be omitted,
/// but required positional arguments may not. All named arguments and explicit
/// ByVal arguments are rejected; zero variadic elements is valid. The supplied
/// arguments must come from a complete bounded [`super::argument_list`] parse.
/// This does not prove expression validity, type compatibility or symbol identity.
///
/// # Errors
/// Returns the caller's cancellation/work error unchanged.
pub fn variadic_arguments_match<E>(
    arguments: &[Argument<'_>],
    required: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    check()?;
    if arguments.len() < required {
        return Ok(false);
    }
    for (position, argument) in arguments.iter().enumerate() {
        check()?;
        let expression = argument.expression();
        if argument.name().is_some()
            || (position < required && expression.is_empty())
            || (expression
                .get(..5)
                .is_some_and(|word| word.eq_ignore_ascii_case(b"ByVal"))
                && expression.get(5).is_some_and(u8::is_ascii_whitespace))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variadic_declarations_require_exact_array_and_variant_shape() {
        for source in [
            "ParamArray items()",
            "paramarray [items] ( ) As [Variant]",
            " ParamArray _\r\n items() As _\n Variant ",
        ] {
            assert_eq!(
                param_array_parameter(source.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                Some(b"items".as_slice()),
                "{source}"
            );
        }
        for source in [
            "[ParamArray] items()",
            "Optional ParamArray items()",
            "ByRef ParamArray items()",
            "ParamArray ByVal items()",
            "ParamArray items",
            "ParamArray items(2)",
            "ParamArray items(,)",
            "ParamArray items%()",
            "ParamArray items() As Long",
            "ParamArray items() As Variant()",
            "ParamArray items() = 0",
            "ParamArray items()As Variant",
            "ParamArray items() [As] Variant",
            "ParamArray items() AsVariant",
            "ParamArray items() ' comment",
            "ParamArray\nitems()",
        ] {
            assert!(
                param_array_parameter(source.as_bytes(), &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn variadic_mapping_preserves_required_slots_and_rejects_named_calls() {
        for (source, required, expected) in [
            ("", 0, true),
            ("", 1, false),
            ("1", 1, true),
            ("1, 2, 3", 1, true),
            ("1,, 3", 1, true),
            (", 2", 1, false),
            (", 2", 0, true),
            ("first:=1", 1, false),
            ("1, tail:=2", 1, false),
            ("ByVal 1", 0, false),
            ("Nested(value:=1), 2", 1, true),
        ] {
            let args = crate::argument_list(source.as_bytes(), 20, &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap();
            assert_eq!(
                variadic_arguments_match(&args, required, &mut || Ok::<(), ()>(())).unwrap(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn variadic_scans_propagate_cancellation_and_accept_arbitrary_bytes_safely() {
        let source = format!("ParamArray {}() As Variant", "a".repeat(8192));
        let mut calls = 0;
        assert_eq!(
            param_array_parameter(source.as_bytes(), &mut || {
                calls += 1;
                if calls == 8 { Err("budget") } else { Ok(()) }
            }),
            Err("budget")
        );
        assert_eq!(calls, 8);
        for byte in 0..=u8::MAX {
            let input = [
                b'P', b'a', b'r', b'a', b'm', b'A', b'r', b'r', b'a', b'y', b' ', byte, b'(', b')',
            ];
            if let Some(name) = param_array_parameter(&input, &mut || Ok::<(), ()>(())).unwrap() {
                assert_eq!(name, &input[11..12]);
            }
            for end in 0..input.len() {
                let _ = param_array_parameter(&input[..end], &mut || Ok::<(), ()>(())).unwrap();
            }
        }
        let args = crate::argument_list(b"1, 2, 3", 3, &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap();
        let mut calls = 0;
        assert_eq!(
            variadic_arguments_match(&args, 0, &mut || {
                calls += 1;
                if calls == 3 { Err("cancelled") } else { Ok(()) }
            }),
            Err("cancelled")
        );
    }
}
