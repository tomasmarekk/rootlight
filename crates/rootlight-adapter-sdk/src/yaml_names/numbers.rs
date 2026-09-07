//! Exact YAML Core numeric normalization without machine-number rounding.
//! Decimal coefficients and exponents stay bounded by the caller's name budget;
//! exponent magnitude never drives allocation or zero expansion.

use crate::json_names::append;

pub(super) enum Number<'a> {
    Integer {
        digits: &'a str,
        radix: u32,
        negative: bool,
    },
    Decimal {
        coefficient: &'a str,
        exponent: &'a str,
        negative: bool,
    },
    Special(&'static str),
}

fn unsigned(text: &str) -> (&str, bool) {
    if let Some(value) = text.strip_prefix('-') {
        (value, true)
    } else {
        (text.strip_prefix('+').unwrap_or(text), false)
    }
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn classify(text: &str) -> Option<Number<'_>> {
    for (prefix, radix) in [("0x", 16), ("0o", 8)] {
        if let Some(value) = text.strip_prefix(prefix)
            && !value.is_empty()
            && value.chars().all(|c| c.is_digit(radix))
        {
            return Some(Number::Integer {
                digits: value,
                radix,
                negative: false,
            });
        }
    }
    let (value, negative) = unsigned(text);
    if digits(value) {
        return Some(Number::Integer {
            digits: value,
            radix: 10,
            negative,
        });
    }
    if matches!(value, ".inf" | ".Inf" | ".INF") {
        return Some(Number::Special(if negative {
            "float:-inf"
        } else {
            "float:inf"
        }));
    }
    if matches!(text, ".nan" | ".NaN" | ".NAN") {
        return Some(Number::Special("float:nan"));
    }
    let (coefficient, exponent) = value.split_once(['e', 'E']).unwrap_or((value, "0"));
    if !digits(unsigned(exponent).0) {
        return None;
    }
    let coefficient_valid = if let Some((whole, fraction)) = coefficient.split_once('.') {
        (whole.is_empty() || digits(whole))
            && (fraction.is_empty() || digits(fraction))
            && !(whole.is_empty() && fraction.is_empty())
    } else {
        digits(coefficient)
    };
    coefficient_valid.then_some(Number::Decimal {
        coefficient,
        exponent,
        negative,
    })
}

impl Number<'_> {
    pub(super) fn canonical(self, maximum: usize) -> Option<String> {
        match self {
            Self::Special(value) => {
                let mut result = String::new();
                append(&mut result, value, maximum)?;
                Some(result)
            }
            Self::Integer {
                digits,
                radix,
                negative,
            } => {
                let mut result = String::new();
                append(&mut result, "int:", maximum)?;
                if radix == 10 {
                    let digits = magnitude(digits);
                    if negative && digits != "0" {
                        append(&mut result, "-", maximum)?;
                    }
                    append(&mut result, digits, maximum)?;
                } else {
                    append_radix_integer(&mut result, digits, radix, maximum)?;
                }
                Some(result)
            }
            Self::Decimal {
                coefficient,
                exponent,
                negative,
            } => {
                let fraction = coefficient
                    .split_once('.')
                    .map_or(0, |(_, part)| part.len());
                let mut coefficient_digits = String::new();
                for character in coefficient.chars().filter(|c| *c != '.') {
                    append(
                        &mut coefficient_digits,
                        character.encode_utf8(&mut [0; 4]),
                        maximum,
                    )?;
                }
                let significant = coefficient_digits.trim_start_matches('0');
                let mut result = String::new();
                append(&mut result, "float:", maximum)?;
                if significant.is_empty() {
                    append(&mut result, "0", maximum)?;
                } else {
                    let normalized = significant.trim_end_matches('0');
                    let removed = significant.len().checked_sub(normalized.len())?;
                    let shift = i128::try_from(removed)
                        .ok()?
                        .checked_sub(i128::try_from(fraction).ok()?)?;
                    let exponent = adjust_exponent(exponent, shift, maximum)?;
                    if negative {
                        append(&mut result, "-", maximum)?;
                    }
                    append(&mut result, normalized, maximum)?;
                    append(&mut result, "e", maximum)?;
                    append(&mut result, &exponent, maximum)?;
                }
                Some(result)
            }
        }
    }
}

fn append_radix_integer(
    result: &mut String,
    digits: &str,
    radix: u32,
    maximum: usize,
) -> Option<()> {
    let chunk_length = match radix {
        16 => 8,
        8 => 10,
        _ => return None,
    };
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return append(result, "0", maximum);
    }
    let remaining = maximum.checked_sub(result.len())?;
    let maximum_limbs = remaining.div_ceil(9);
    // The release benchmark yaml_radix exposes per-digit quadratic work.
    // Each operation here consumes up to 32 input bits and nine decimal digits;
    // (10^9 - 1) * 2^32 + (2^32 - 1) fits u64 without rounding.
    const DECIMAL_BASE: u64 = 1_000_000_000;
    let mut decimal = Vec::<u32>::new();
    for chunk in digits.as_bytes().chunks(chunk_length) {
        let text = std::str::from_utf8(chunk).ok()?;
        let mut carry = u64::from(u32::from_str_radix(text, radix).ok()?);
        let multiplier = u64::from(radix).checked_pow(u32::try_from(chunk.len()).ok()?)?;
        for limb in &mut decimal {
            let value = u64::from(*limb)
                .checked_mul(multiplier)?
                .checked_add(carry)?;
            *limb = u32::try_from(value % DECIMAL_BASE).ok()?;
            carry = value / DECIMAL_BASE;
        }
        while carry > 0 {
            if decimal.len() >= maximum_limbs {
                return None;
            }
            decimal.try_reserve(1).ok()?;
            decimal.push(u32::try_from(carry % DECIMAL_BASE).ok()?);
            carry /= DECIMAL_BASE;
        }
    }
    let (&first, rest) = decimal.split_last()?;
    let first = first.to_string();
    let bytes = rest.len().checked_mul(9)?.checked_add(first.len())?;
    if bytes > remaining {
        return None;
    }
    result.try_reserve_exact(bytes).ok()?;
    append(result, &first, maximum)?;
    for &limb in rest.iter().rev() {
        let mut value = limb;
        let mut buffer = [b'0'; 9];
        for digit in buffer.iter_mut().rev() {
            *digit = b'0'.checked_add(u8::try_from(value % 10).ok()?)?;
            value /= 10;
        }
        append(result, std::str::from_utf8(&buffer).ok()?, maximum)?;
    }
    Some(())
}

fn magnitude(value: &str) -> &str {
    let significant = value.trim_start_matches('0');
    if significant.is_empty() {
        "0"
    } else {
        significant
    }
}

fn adjust_exponent(text: &str, shift: i128, maximum: usize) -> Option<String> {
    let (value, negative) = unsigned(text);
    let value = magnitude(value);
    let delta = shift.unsigned_abs().to_string();
    let (large, small, subtract, result_negative) = if negative == (shift < 0) {
        (value, delta.as_str(), false, negative)
    } else if (value.len(), value) >= (delta.len(), delta.as_str()) {
        (value, delta.as_str(), true, negative)
    } else {
        (delta.as_str(), value, true, shift < 0)
    };
    let mut left = large.bytes().rev();
    let mut right = small.bytes().rev();
    let mut reversed = Vec::<u8>::new();
    let mut carry = 0i16;
    loop {
        let a = left.next();
        let b = right.next();
        if a.is_none() && b.is_none() && carry == 0 {
            break;
        }
        let a = i16::from(a.map_or(0, |byte| byte - b'0'));
        let b = i16::from(b.map_or(0, |byte| byte - b'0'));
        let total = if subtract {
            a - b + carry
        } else {
            a + b + carry
        };
        carry = if subtract {
            if total < 0 { -1 } else { 0 }
        } else {
            total / 10
        };
        let digit = u8::try_from(total.rem_euclid(10)).ok()?;
        if reversed.len() >= maximum {
            return None;
        }
        reversed.try_reserve(1).ok()?;
        reversed.push(digit);
    }
    while reversed.len() > 1 && reversed.last() == Some(&0) {
        reversed.pop();
    }
    let mut result = String::new();
    if result_negative && reversed.iter().any(|digit| *digit != 0) {
        append(&mut result, "-", maximum)?;
    }
    for digit in reversed.into_iter().rev() {
        append(
            &mut result,
            char::from(b'0' + digit).encode_utf8(&mut [0; 4]),
            maximum,
        )?;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponent_adjustment_matches_integer_arithmetic_with_both_signs() {
        for value in -300i128..=300 {
            for shift in [-301, -100, -1, 0, 1, 100, 301] {
                assert_eq!(
                    adjust_exponent(&value.to_string(), shift, 32).unwrap(),
                    (value + shift).to_string()
                );
            }
        }
    }
}
