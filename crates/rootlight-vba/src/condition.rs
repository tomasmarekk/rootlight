//! Canonical spelling for pure compiler-constant Boolean expressions.
//! Keys preserve operators and grouping; callers must separately prove that
//! both expressions observe the same compiler-constant environment.

use super::{logical_whitespace_len, name};

/// Produces a case-insensitive token key for a compiler-constant expression.
///
/// Accepts ASCII names, parentheses, `Not`, and binary `And`, `Or`, `Xor`,
/// `Eqv` and `Imp`. This is spelling equivalence, not evaluation or Boolean
/// simplification or validation of the names' bindings. Names include VBA's
/// word-form literal spellings. Numeric, string and date literals,
/// calls, member access, type suffixes, bracketed operator/literal names and
/// malformed expressions return `None`.
/// The caller must prove a common, unambiguous compilation environment before
/// equal keys imply equal results. `maximum_name_bytes` bounds each name.
/// Parsing is iterative, cancellable and uses at most twice the input length
/// for key bytes; parentheses require no stack allocation.
///
/// # Errors
/// Returns the caller's cancellation/work error unchanged.
pub fn compilation_condition_key<E>(
    source: &[u8],
    maximum_name_bytes: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Vec<u8>>, E> {
    check()?;
    let mut cursor = 0;
    let mut depth = 0_usize;
    let mut operand = true;
    let mut key = Vec::new();
    while cursor < source.len() {
        cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
        check()?;
        let Some(&byte) = source.get(cursor) else {
            break;
        };
        match byte {
            b'(' if operand => {
                depth += 1;
                key.push(byte);
                cursor += 1;
            }
            b')' if !operand && depth != 0 => {
                depth -= 1;
                key.push(byte);
                cursor += 1;
            }
            _ => {
                let bracketed = byte == b'[';
                let Some(word) = name(source, &mut cursor, check)? else {
                    return Ok(None);
                };
                if word.len() > maximum_name_bytes {
                    return Ok(None);
                }
                let unary = word.eq_ignore_ascii_case(b"Not");
                let binary = [b"And".as_slice(), b"Or", b"Xor", b"Eqv", b"Imp"]
                    .iter()
                    .any(|operator| word.eq_ignore_ascii_case(operator));
                if bracketed
                    && (unary
                        || binary
                        || [b"True".as_slice(), b"False", b"Empty", b"Null", b"Nothing"]
                            .iter()
                            .any(|literal| word.eq_ignore_ascii_case(literal)))
                {
                    return Ok(None);
                }
                if (operand && binary) || (!operand && !binary) {
                    return Ok(None);
                }
                operand = binary || unary;
                // A separator preserves token boundaries independently of the
                // authored whitespace, including continuation lines.
                key.extend(word.iter().map(u8::to_ascii_lowercase));
                key.push(0);
            }
        }
    }
    Ok((!operand && depth == 0 && !key.is_empty()).then_some(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(source: &[u8]) -> Option<Vec<u8>> {
        compilation_condition_key(source, 128, &mut || Ok::<(), ()>(())).unwrap()
    }

    #[test]
    fn condition_keys_preserve_grouping_and_operator_order() {
        assert_eq!(
            key(b"Feature Or Not Legacy"),
            key(b"fEaTuRe _\r\n OR\tNot [legacy]")
        );
        assert_eq!(key(b"(A And B) Or C"), key(b"( a AND b )OR c"));
        assert_ne!(key(b"(A And B) Or C"), key(b"A And (B Or C)"));
        assert_ne!(key(b"A Or B"), key(b"B Or A"));
        assert_ne!(key(b"A And B"), key(b"A Or B"));
        for source in [
            b"Not Not A".as_slice(),
            b"True",
            b"False",
            b"A Xor B Eqv C Imp D",
        ] {
            assert!(key(source).is_some());
        }
    }

    #[test]
    fn unsupported_or_incomplete_conditions_do_not_have_keys() {
        for source in [
            "",
            " ",
            "()",
            "(A",
            "A)",
            "A B",
            "A Not B",
            "And A",
            "A Or",
            "Not",
            "A()",
            "A.B",
            "1",
            "A = B",
            "A%",
            "A\nOr B",
            "A 'comment",
            "A: B",
            "\"A\"",
            "Not(A)(B)",
            "[True]",
            "[False]",
            "[Not] A",
            "A [And] B",
        ] {
            assert!(key(source.as_bytes()).is_none(), "{source:?}");
        }
        assert!(
            compilation_condition_key(b"LongName", 3, &mut || Ok::<(), ()>(()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn condition_scans_cancel_and_accept_arbitrary_bytes_without_panics() {
        let input = format!("{}Feature{}", "(".repeat(4096), ")".repeat(4096));
        let mut calls = 0;
        assert_eq!(
            compilation_condition_key(input.as_bytes(), 128, &mut || {
                calls += 1;
                if calls == 100 {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            }),
            Err("cancelled")
        );
        for byte in 0..=u8::MAX {
            let input = [b"(A Or ".as_slice(), &[byte], b")"].concat();
            for length in 0..=input.len() {
                if let Some(value) = key(&input[..length]) {
                    assert!(value.len() <= length * 2);
                }
            }
        }
    }
}
