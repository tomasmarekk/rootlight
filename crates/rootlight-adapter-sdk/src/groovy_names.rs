//! Constant Groovy member names without evaluation or Unicode normalization.
//! Ordered escape passes retain Java UTF-16 units; unusual names use an injective
//! quoted identity instead of losing controls, empty strings or lone surrogates.

use std::{borrow::Cow, iter::Peekable, slice::Iter};

use unicode_general_category::{GeneralCategory, get_general_category};

use crate::json_names::{append, append_character, append_unit_escape};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quoting {
    Single,
    Double,
    Slashy,
    DollarSlashy,
}

pub(crate) fn canonical_groovy_name(text: &str, maximum: usize) -> Option<Cow<'_, str>> {
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    let (opener, closer, quoting, multiline) = if text.starts_with("'''") {
        ("'''", "'''", Quoting::Single, true)
    } else if text.starts_with("\"\"\"") {
        ("\"\"\"", "\"\"\"", Quoting::Double, true)
    } else if text.starts_with("$/") {
        ("$/", "/$", Quoting::DollarSlashy, true)
    } else if text.starts_with('\'') {
        ("'", "'", Quoting::Single, false)
    } else if text.starts_with('"') {
        ("\"", "\"", Quoting::Double, false)
    } else if text.starts_with('/') {
        ("/", "/", Quoting::Slashy, true)
    } else {
        return crate::structural_captured_name(text, maximum).map(Cow::Borrowed);
    };
    let body = text.strip_prefix(opener)?.strip_suffix(closer)?;
    if !valid_body(body, closer, quoting, multiline) {
        return None;
    }
    if !body.contains(['\\', '$', '\r']) && !needs_quoted_identity(body) {
        return Some(Cow::Borrowed(body));
    }
    let mut units = Vec::new();
    units.try_reserve_exact(body.len()).ok()?;
    units.extend(body.encode_utf16());
    if multiline {
        units = replace_pair(&units, b'\r', b'\n', b'\n')?;
    }
    // Groovy applies these passes in order, so a decoded backslash can introduce
    // a later escape. A single combined unescape pass would change that meaning.
    if matches!(quoting, Quoting::Single | Quoting::Double) {
        units = replace_pair(&units, b'\\', b'$', b'$')?;
        units = escape_pass(&units, EscapePass::Line)?;
        units = escape_pass(&units, EscapePass::Octal)?;
        units = escape_pass(&units, EscapePass::Unicode)?;
        units = escape_pass(&units, EscapePass::Standard)?;
        units = replace_pair(&units, b'\\', b'\\', b'\\')?;
    } else {
        units = escape_pass(&units, EscapePass::Unicode)?;
        units = escape_pass(&units, EscapePass::Line)?;
        if quoting == Quoting::Slashy {
            units = replace_pair(&units, b'\\', b'/', b'/')?;
        } else {
            units = replace_pair(&units, b'$', b'/', b'/')?;
            units = replace_pair(&units, b'$', b'$', b'$')?;
        }
    }
    canonical_units(&units, maximum).map(Cow::Owned)
}

fn valid_body(body: &str, closer: &str, quoting: Quoting, multiline: bool) -> bool {
    let mut chars = body.chars();
    while !chars.as_str().is_empty() {
        if chars.as_str().starts_with(closer) {
            return false;
        }
        let Some(ch) = chars.next() else { break };
        if !multiline && matches!(ch, '\r' | '\n') {
            return false;
        }
        if ch == '\\' {
            if matches!(quoting, Quoting::Single | Quoting::Double) {
                match chars.next() {
                    Some('b' | 't' | 'n' | 'f' | 'r' | 's' | '\'' | '"' | '\\' | '$' | '\n') => {}
                    Some('\r') if chars.next() == Some('\n') => {}
                    Some('u') => {
                        for _ in 0..4 {
                            if !chars.next().is_some_and(|digit| digit.is_ascii_hexdigit()) {
                                return false;
                            }
                        }
                    }
                    Some(first @ '0'..='7') => {
                        for _ in 0..if first <= '3' { 2 } else { 1 } {
                            if chars
                                .as_str()
                                .starts_with(['0', '1', '2', '3', '4', '5', '6', '7'])
                            {
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }
                    _ => return false,
                }
            } else if quoting == Quoting::Slashy && chars.as_str().starts_with('/') {
                chars.next();
            }
        } else if ch == '$' && quoting != Quoting::Single {
            let next = chars.clone().next();
            if quoting == Quoting::DollarSlashy && matches!(next, Some('$' | '/')) {
                chars.next();
            } else if quoting == Quoting::Double || next.is_some_and(interpolation_start) {
                return false;
            }
        }
    }
    true
}

fn interpolation_start(ch: char) -> bool {
    ch == '{'
        || (ch != '$'
            && matches!(
                get_general_category(ch),
                GeneralCategory::UppercaseLetter
                    | GeneralCategory::LowercaseLetter
                    | GeneralCategory::TitlecaseLetter
                    | GeneralCategory::ModifierLetter
                    | GeneralCategory::OtherLetter
                    | GeneralCategory::LetterNumber
                    | GeneralCategory::CurrencySymbol
                    | GeneralCategory::ConnectorPunctuation
            ))
}

#[derive(Clone, Copy)]
enum EscapePass {
    Line,
    Octal,
    Unicode,
    Standard,
}

type Units<'a> = Peekable<std::iter::Copied<Iter<'a, u16>>>;

fn escape_pass(input: &[u16], pass: EscapePass) -> Option<Vec<u16>> {
    let mut output = Vec::new();
    output.try_reserve_exact(input.len()).ok()?;
    let mut units = input.iter().copied().peekable();
    while let Some(unit) = units.next() {
        if unit != u16::from(b'\\') {
            output.push(unit);
            continue;
        }
        let mut count = 1usize;
        while units.peek() == Some(&unit) {
            units.next();
            count = count.checked_add(1)?;
        }
        let mut tail = units.clone();
        if count % 2 == 1
            && let Some(replacement) = escape_value(&mut tail, pass)
        {
            output.extend(std::iter::repeat_n(unit, count - 1));
            output.extend(replacement);
            units = tail;
        } else {
            output.extend(std::iter::repeat_n(unit, count));
        }
    }
    Some(output)
}

fn escape_value(units: &mut Units<'_>, pass: EscapePass) -> Option<Option<u16>> {
    let first = char::from_u32(u32::from(units.next()?))?;
    Some(match pass {
        EscapePass::Line => {
            if first != '\n' && !(first == '\r' && units.next()? == u16::from(b'\n')) {
                return None;
            }
            None
        }
        EscapePass::Unicode => {
            if first != 'u' {
                return None;
            }
            let mut value = 0u16;
            for _ in 0..4 {
                let digit = char::from_u32(u32::from(units.next()?))?.to_digit(16)?;
                value = value
                    .checked_mul(16)?
                    .checked_add(u16::try_from(digit).ok()?)?;
            }
            Some(value)
        }
        EscapePass::Octal => {
            let mut value = u16::try_from(first.to_digit(8)?).ok()?;
            for _ in 0..if first <= '3' { 2 } else { 1 } {
                let Some(digit) = units
                    .peek()
                    .and_then(|unit| char::from_u32(u32::from(*unit)))
                    .and_then(|ch| ch.to_digit(8))
                else {
                    break;
                };
                units.next();
                value = value
                    .checked_mul(8)?
                    .checked_add(u16::try_from(digit).ok()?)?;
            }
            Some(value)
        }
        EscapePass::Standard => Some(u16::from(match first {
            'b' => b'\x08',
            't' => b'\t',
            'n' => b'\n',
            'f' => b'\x0c',
            'r' => b'\r',
            's' => b' ',
            '\'' => b'\'',
            '"' => b'"',
            _ => return None,
        })),
    })
}

fn replace_pair(input: &[u16], first: u8, second: u8, replacement: u8) -> Option<Vec<u16>> {
    let mut output = Vec::new();
    output.try_reserve_exact(input.len()).ok()?;
    let mut units = input.iter().copied().peekable();
    while let Some(unit) = units.next() {
        if unit == u16::from(first) && units.peek() == Some(&u16::from(second)) {
            units.next();
            output.push(u16::from(replacement));
        } else {
            output.push(unit);
        }
    }
    Some(output)
}

fn needs_quoted_identity(name: &str) -> bool {
    name.is_empty()
        || name.starts_with('"')
        || name.trim() != name
        || name.chars().any(char::is_control)
}

fn canonical_units(units: &[u16], maximum: usize) -> Option<String> {
    let mut decoded = String::new();
    for ch in char::decode_utf16(units.iter().copied()) {
        let Ok(ch) = ch else {
            return quoted_units(units, maximum);
        };
        append(&mut decoded, ch.encode_utf8(&mut [0; 4]), maximum)?;
    }
    if needs_quoted_identity(&decoded) {
        quoted_units(units, maximum)
    } else {
        Some(decoded)
    }
}

fn quoted_units(units: &[u16], maximum: usize) -> Option<String> {
    let mut quoted = String::new();
    append(&mut quoted, "\"", maximum)?;
    for ch in char::decode_utf16(units.iter().copied()) {
        match ch {
            Ok(ch) => append_character(&mut quoted, ch, maximum)?,
            Err(error) => append_unit_escape(&mut quoted, error.unpaired_surrogate(), maximum)?,
        }
    }
    append(&mut quoted, "\"", maximum)?;
    Some(quoted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groovy_escape_pass_order_and_literal_backslashes_match_conversion() {
        for (source, expected) in [
            (r"'unicode\u005cn'", r#""unicode\u000a""#),
            (r"'octal\134n'", r#""octal\u000a""#),
            (r"'octal\134u0061'", "octala"),
            (r"'literal\\u0061'", r"literal\u0061"),
            ("'''a\\\r\nb'''", "ab"),
            ("'''a\r\nb'''", r#""a\u000ab""#),
            (r"$/hex\u0024\u0024name/$", "hex$name"),
        ] {
            assert_eq!(
                canonical_groovy_name(source, 128).as_deref(),
                Some(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn groovy_names_reject_dynamic_invalid_and_oversized_captures_without_prefixes() {
        for source in [
            r#""${name}""#,
            r#""$name""#,
            r#""$5""#,
            r#""""$name""""#,
            "/$name/",
            "$/prefix${name}/$",
            "$/$$$name/$",
            "/$£value/",
            "/$‿value/",
            r"'bad\q'",
            r"'bad\uu0061'",
            r"'bad\/slash'",
            "'a'b'",
            "'unterminated",
            "'''a'''b'''",
            "'a\nb'",
            r"'bad\u123'",
        ] {
            assert!(canonical_groovy_name(source, 128).is_none(), "{source}");
        }
        for source in ["'with space'", "\"🚀\"", r"'\uD83D\uDE80'", "$/$$name/$"] {
            assert!(
                canonical_groovy_name(source, source.len()).is_some(),
                "{source}"
            );
            assert!(
                canonical_groovy_name(source, source.len() - 1).is_none(),
                "{source}"
            );
        }
        assert!(
            canonical_groovy_name(r"'\n'", 4).is_none(),
            "canonical escaped output also has a budget"
        );
        assert_eq!(canonical_groovy_name("'a\"b'", 5).as_deref(), Some("a\"b"));
    }

    #[test]
    fn groovy_names_do_not_collapse_special_names_or_normalize_unicode() {
        for (left, right) in [
            (r"'\uD800'", r"'\\ud800'"),
            (r"'\uD800'", "'�'"),
            (r"'\n'", r"'\\u000a'"),
            ("''", "' '"),
            ("'é'", r"'e\u0301'"),
            (r#"'"\n"'"#, r"'\n'"),
        ] {
            assert_ne!(
                canonical_groovy_name(left, 128),
                canonical_groovy_name(right, 128)
            );
        }
        for source in [
            "lookup",
            "'lookup'",
            "\"lookup\"",
            r"'look\u0075p'",
            "/lookup/",
            "$/lookup/$",
        ] {
            assert_eq!(
                canonical_groovy_name(source, 128).as_deref(),
                Some("lookup")
            );
        }
    }
}
