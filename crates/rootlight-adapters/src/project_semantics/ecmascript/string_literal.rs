//! Strict ECMAScript string values for source-backed module and export names.
//! Decoding never changes source spans; non-scalar UTF-16 values remain unavailable
//! because project names are UTF-8 and must not contain replacement characters.

use std::{iter::Peekable, str::Chars};

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;

pub(super) fn decode(
    source: &str,
    cancellation: &Cancellation,
) -> Result<Option<String>, AdapterError> {
    cancellation.check()?;
    let Some(quote @ ('\'' | '"')) = source.chars().next() else {
        return Ok(None);
    };
    let Some(inner) = source
        .strip_prefix(quote)
        .and_then(|text| text.strip_suffix(quote))
    else {
        return Ok(None);
    };
    let mut chars = inner.chars().peekable();
    let mut output = String::new();
    let mut high_surrogate = None;
    while let Some(character) = chars.next() {
        cancellation.check()?;
        let code = if character == '\\' {
            let Some(escaped) = chars.next() else {
                return Ok(None);
            };
            match escaped {
                '\n' | '\u{2028}' | '\u{2029}' => continue,
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    continue;
                }
                'b' => 0x08,
                't' => 0x09,
                'n' => 0x0a,
                'v' => 0x0b,
                'f' => 0x0c,
                'r' => 0x0d,
                '0' if chars.peek().is_none_or(|next| !next.is_ascii_digit()) => 0,
                '0'..='9' => return Ok(None),
                'x' => {
                    let Some(value) = hex_digits(&mut chars, 2) else {
                        return Ok(None);
                    };
                    value
                }
                'u' => {
                    let Some(value) = unicode_escape(&mut chars, cancellation)? else {
                        return Ok(None);
                    };
                    value
                }
                other => u32::from(other),
            }
        } else if character == quote || matches!(character, '\r' | '\n') {
            return Ok(None);
        } else {
            u32::from(character)
        };
        if append_codepoint(&mut output, &mut high_surrogate, code).is_none() {
            return Ok(None);
        }
    }
    Ok(high_surrogate.is_none().then_some(output))
}

fn hex_digits(chars: &mut Peekable<Chars<'_>>, count: usize) -> Option<u32> {
    let mut value = 0_u32;
    for _ in 0..count {
        value = value
            .checked_mul(16)?
            .checked_add(chars.next()?.to_digit(16)?)?;
    }
    Some(value)
}

fn unicode_escape(
    chars: &mut Peekable<Chars<'_>>,
    cancellation: &Cancellation,
) -> Result<Option<u32>, AdapterError> {
    if chars.peek() != Some(&'{') {
        return Ok(hex_digits(chars, 4));
    }
    chars.next();
    let mut value = 0_u32;
    let mut has_digit = false;
    for character in chars.by_ref() {
        cancellation.check()?;
        if character == '}' {
            return Ok(has_digit.then_some(value));
        }
        let Some(next) = character
            .to_digit(16)
            .and_then(|digit| value.checked_mul(16)?.checked_add(digit))
            .filter(|value| *value <= 0x10_ffff)
        else {
            return Ok(None);
        };
        value = next;
        has_digit = true;
    }
    Ok(None)
}

fn append_codepoint(
    output: &mut String,
    high_surrogate: &mut Option<u32>,
    code: u32,
) -> Option<()> {
    if let Some(high) = high_surrogate.take() {
        if !(0xdc00..=0xdfff).contains(&code) {
            return None;
        }
        // The two checked surrogate ranges bound this UTF-16 reconstruction
        // to a Unicode scalar at or below U+10FFFF.
        let scalar = 0x10000 + ((high - 0xd800) << 10) + (code - 0xdc00);
        output.push(char::from_u32(scalar)?);
    } else if (0xd800..=0xdbff).contains(&code) {
        *high_surrogate = Some(code);
    } else {
        output.push(char::from_u32(code)?);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn import_literal_values_match_reviewed_strict_ecmascript_cases() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ecmascript_import_literals.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let literal = case["literal"].as_str().unwrap();
            let actual = decode(literal, &Cancellation::new()).unwrap();
            assert_eq!(
                actual.as_deref(),
                case["value"].as_str(),
                "{}: {literal:?}",
                case["name"]
            );
        }
    }

    #[test]
    fn unicode_scalar_spellings_preserve_exact_values() {
        for scalar in [
            0, 0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xffff, 0x10000, 0x10ffff,
        ] {
            let character = char::from_u32(scalar).unwrap();
            let value = character.to_string();
            let units: String = value
                .encode_utf16()
                .map(|unit| format!("\\u{unit:04x}"))
                .collect();
            for literal in [
                format!("'{units}'"),
                format!("'\\u{{{scalar:x}}}'"),
                serde_json::to_string(&value).unwrap(),
            ] {
                assert_eq!(
                    decode(&literal, &Cancellation::new()).unwrap().as_deref(),
                    Some(value.as_str())
                );
            }
        }
    }

    #[test]
    fn cancelled_literal_decoding_preserves_the_cancellation_error() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            decode("'\\u{00000061}'", &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
