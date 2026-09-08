//! Bounded decoding of grammar-reviewed R names, independent of runtime evaluation.
//! Byte escapes require UTF-8 source identity; undecodable bytes and unpaired
//! surrogates remain unavailable rather than becoming replacement characters.

use std::{borrow::Cow, iter::Peekable, str::Chars};

pub(crate) fn canonical_r_name(text: &str, maximum: usize) -> Option<Cow<'_, str>> {
    if text.is_empty() || text.len() > maximum || text.contains('\0') {
        return None;
    }
    if text.starts_with(['r', 'R']) && matches!(text.as_bytes().get(1), Some(b'\'' | b'"')) {
        return raw_name(text).map(Cow::Borrowed);
    }
    let quote = text.chars().next()?;
    if !matches!(quote, '`' | '\'' | '"') {
        // This API consumes reviewed grammar captures, not arbitrary R programs.
        return Some(Cow::Borrowed(text));
    }
    let body = text.strip_prefix(quote)?.strip_suffix(quote)?;
    if body.is_empty() {
        return None;
    }
    if !body.contains('\\') {
        return (!body.contains(quote) && !body.chars().any(forbidden_literal))
            .then_some(Cow::Borrowed(body));
    }
    let mut characters = body.chars().peekable();
    let mut output = Vec::new();
    let mut byte_escapes = false;
    let mut unicode_escapes = false;
    while let Some(character) = characters.next() {
        if character == quote {
            return None;
        }
        let character = if character == '\\' {
            match characters.next()? {
                first @ '0'..='7' => {
                    byte_escapes = true;
                    let mut value = first.to_digit(8)?;
                    for _ in 0..2 {
                        let Some(digit) = characters.peek().and_then(|c| c.to_digit(8)) else {
                            break;
                        };
                        characters.next();
                        value = value.checked_mul(8)?.checked_add(digit)?;
                    }
                    append(&mut output, &[u8::try_from(value).ok()?], maximum)?;
                    continue;
                }
                'x' => {
                    byte_escapes = true;
                    let value = digits(&mut characters, 2)?;
                    append(&mut output, &[u8::try_from(value).ok()?], maximum)?;
                    continue;
                }
                marker @ ('u' | 'U') => {
                    // R rejects Unicode escapes in backticks and mixed byte/Unicode escapes.
                    if quote == '`' {
                        return None;
                    }
                    unicode_escapes = true;
                    let mut scalar = unicode(&mut characters, marker)?;
                    if (0xd800..=0xdbff).contains(&scalar) {
                        if characters.next()? != '\\' {
                            return None;
                        }
                        let marker = characters.next()?;
                        if !matches!(marker, 'u' | 'U') {
                            return None;
                        }
                        let low = unicode(&mut characters, marker)?;
                        if !(0xdc00..=0xdfff).contains(&low) {
                            return None;
                        }
                        scalar = 0x10000 + ((scalar - 0xd800) << 10) + (low - 0xdc00);
                    }
                    char::from_u32(scalar)?
                }
                'a' => '\u{7}',
                'b' => '\u{8}',
                'f' => '\u{c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'v' => '\u{b}',
                value @ ('\\' | '\'' | '"' | '`' | ' ' | '\n') => value,
                _ => return None,
            }
        } else {
            if forbidden_literal(character) {
                return None;
            }
            character
        };
        let mut bytes = [0; 4];
        append(
            &mut output,
            character.encode_utf8(&mut bytes).as_bytes(),
            maximum,
        )?;
    }
    if byte_escapes && unicode_escapes {
        return None;
    }
    let output = String::from_utf8(output).ok()?;
    (!output.is_empty()).then_some(Cow::Owned(output))
}

fn append(output: &mut Vec<u8>, bytes: &[u8], maximum: usize) -> Option<()> {
    if bytes.contains(&0) || output.len().checked_add(bytes.len())? > maximum {
        return None;
    }
    output.try_reserve(bytes.len()).ok()?;
    output.extend_from_slice(bytes);
    Some(())
}

fn digits(characters: &mut Peekable<Chars<'_>>, maximum: usize) -> Option<u32> {
    let mut value = 0_u32;
    let mut count = 0;
    for _ in 0..maximum {
        let Some(digit) = characters.peek().and_then(|c| c.to_digit(16)) else {
            break;
        };
        characters.next();
        value = value.checked_mul(16)?.checked_add(digit)?;
        count += 1;
    }
    (count != 0).then_some(value)
}

fn unicode(characters: &mut Peekable<Chars<'_>>, marker: char) -> Option<u32> {
    let braced = characters.peek() == Some(&'{');
    if braced {
        characters.next();
    }
    let value = digits(characters, if marker == 'u' { 4 } else { 8 })?;
    if braced && characters.next()? != '}' {
        return None;
    }
    (value != 0 && value <= 0x10ffff).then_some(value)
}

fn raw_name(text: &str) -> Option<&str> {
    let text = text.get(1..)?;
    let quote = text.chars().next()?;
    let text = text.strip_prefix(quote)?;
    let after_dashes = text.trim_start_matches('-');
    let dashes = text.len().checked_sub(after_dashes.len())?;
    let open = after_dashes.chars().next()?;
    let close = match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => return None,
    };
    let body = after_dashes.strip_prefix(open)?;
    let mut ending = String::new();
    ending.try_reserve_exact(dashes.checked_add(2)?).ok()?;
    ending.push(close);
    for _ in 0..dashes {
        ending.push('-');
    }
    ending.push(quote);
    let name = body.strip_suffix(&ending)?;
    (!name.is_empty() && !name.contains(&ending) && !name.chars().any(forbidden_literal))
        .then_some(name)
}

fn forbidden_literal(character: char) -> bool {
    matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_scalar_escapes_match_utf8_bytes_without_normalizing_names() {
        for scalar in (1..=0xffff).chain([0x10000, 0x1d11e, 0x1f680, 0x10ffff]) {
            let Some(character) = char::from_u32(scalar) else {
                continue;
            };
            let name = character.to_string();
            for source in [
                format!(r#""\U{scalar:08x}""#),
                format!(r#""\U{{{scalar:x}}}""#),
            ] {
                assert_eq!(
                    canonical_r_name(&source, 128).as_deref(),
                    Some(name.as_str()),
                    "{source}"
                );
            }
            let escaped = name
                .as_bytes()
                .iter()
                .map(|byte| format!("\\x{byte:02x}"))
                .collect::<String>();
            assert_eq!(
                canonical_r_name(&format!("\"{escaped}\""), 128).as_deref(),
                Some(name.as_str())
            );
        }
        assert_eq!(
            canonical_r_name(r#""\uD834\uDD1E""#, 128).as_deref(),
            Some("𝄞")
        );
        assert_ne!(
            canonical_r_name("`é`", 128),
            canonical_r_name("`e\u{301}`", 128)
        );
    }

    #[test]
    fn raw_delimiters_and_escape_boundaries_are_exact() {
        for prefix in ['r', 'R'] {
            for quote in ['\'', '"'] {
                for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
                    for dashes in ["", "-", "---"] {
                        let source =
                            format!("{prefix}{quote}{dashes}{open}value\\n{close}{dashes}{quote}");
                        assert_eq!(
                            canonical_r_name(&source, source.len()).as_deref(),
                            Some(r"value\n")
                        );
                        assert!(canonical_r_name(&source, source.len() - 1).is_none());
                    }
                }
            }
        }
        for source in [
            r#"r"(early)"late)""#,
            r#"r"--(bad)-""#,
            r#""\u{12345}""#,
            r#""\u{}""#,
            r#""\U00110000""#,
            r#""\x0""#,
            r#""\u0061\x62""#,
            r#""\x61\u0062""#,
            r#""\uD800x""#,
            r#""\uDC00""#,
            r#""\uD800\u0061""#,
        ] {
            assert!(canonical_r_name(source, 128).is_none(), "{source}");
        }
        assert_eq!(
            canonical_r_name(r#""\1412\x62f""#, 128).as_deref(),
            Some("a2bf")
        );
        assert_eq!(canonical_r_name("`a\\ b`", 128).as_deref(), Some("a b"));
    }
}
