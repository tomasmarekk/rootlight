//! Reads source-written parameter modifiers independently of parser captures.
//! Array eligibility is narrower than syntactic parameter recognition: ByVal
//! and incompatible modifier combinations cannot establish an array binding.

use super::logical_whitespace_len;

/// Checks whether a complete parameter-modifier region permits an array.
///
/// The caller supplies only the source before the parameter name. Empty input
/// means implicit ByRef. ByRef and Optional may each occur once, in either order;
/// ParamArray must occur alone. ByVal, duplicates and other tokens return false.
/// Space/tab-based logical continuations are accepted, but bare newlines and
/// comments are not. Matching is ASCII case-insensitive and allocation-free.
/// The parameter's name, empty array designator, declared type and surrounding
/// parameter-list constraints remain caller obligations.
///
/// # Errors
/// Returns the caller's error unchanged when its cancellation/work callback fails.
pub fn array_parameter_modifiers<E>(
    source: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let mut rest = source;
    let mut by_ref = false;
    let mut optional = false;
    let mut variadic = false;
    loop {
        let whitespace = logical_whitespace_len(rest, check)?;
        rest = rest.get(whitespace..).unwrap_or_default();
        if rest.is_empty() {
            return Ok(true);
        }
        let length = if rest
            .get(..5)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"ByRef"))
        {
            if by_ref || variadic {
                return Ok(false);
            }
            by_ref = true;
            5
        } else if rest
            .get(..8)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"Optional"))
        {
            if optional || variadic {
                return Ok(false);
            }
            optional = true;
            8
        } else if rest
            .get(..10)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"ParamArray"))
        {
            if by_ref || optional || variadic {
                return Ok(false);
            }
            variadic = true;
            10
        } else {
            return Ok(false);
        };
        rest = rest.get(length..).unwrap_or_default();
        if !rest.is_empty() && !matches!(rest.first(), Some(b' ' | b'\t')) {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_modifiers_keep_order_alternatives_and_logical_lines() {
        for input in [
            "",
            " \t",
            "ByRef",
            "Optional",
            "ParamArray",
            "Optional ByRef",
            "ByRef Optional",
            " bYrEf _\r\n Optional _\n ",
            "ParamArray _\n ",
        ] {
            assert!(
                array_parameter_modifiers(input.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                "{input:?}"
            );
        }
        for input in [
            "ByVal",
            "Optional ByVal",
            "ByRef ByRef",
            "Optional Optional",
            "Optional ParamArray",
            "ParamArray Optional",
            "ParamArray ByRef",
            "ByRef ParamArray",
            "ParamArray ParamArray",
            "ByRefOptional",
            "ByRef_",
            "ByRef \n",
            "ByRef _ ",
            "ByRef ' note",
            "ByRef :",
            "ByRefX",
        ] {
            assert!(
                !array_parameter_modifiers(input.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn modifier_reads_propagate_cancellation_and_reject_arbitrary_tokens() {
        for input in [b"ByRef ".to_vec(), vec![b' '; 8192], b" _\r\n".repeat(8192)] {
            assert_eq!(
                array_parameter_modifiers(&input, &mut || Err("cancelled")),
                Err("cancelled")
            );
        }
        let mut calls = 0;
        assert_eq!(
            array_parameter_modifiers(&vec![b' '; 8192], &mut || {
                calls += 1;
                if calls == 5 { Err("budget") } else { Ok(()) }
            }),
            Err("budget")
        );
        assert_eq!(calls, 5);
        for byte in 0..=u8::MAX {
            let input = [b'B', b'y', b'R', b'e', b'f', byte];
            let valid = array_parameter_modifiers(&input, &mut || Ok::<(), ()>(())).unwrap();
            assert_eq!(valid, matches!(byte, b' ' | b'\t'));
        }
    }
}
