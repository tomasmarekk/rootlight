//! Readable names for source-backed PowerShell literal data keys.
//! Written spelling remains the durable identity; display decoding never evaluates
//! interpolation or claims runtime key equality, culture or case normalization.

use std::{borrow::Cow, str::Chars};

pub(crate) fn display_key(text: &str) -> Option<Cow<'_, str>> {
    let (body, quote, here) = if let Some(rest) = text.strip_prefix("@'") {
        (here_body(rest, "'@")?, '\'', true)
    } else if let Some(rest) = text.strip_prefix("@\"") {
        (here_body(rest, "\"@")?, '"', true)
    } else if let Some(rest) = text.strip_prefix('\'') {
        (rest.strip_suffix('\'')?, '\'', false)
    } else {
        let rest = text.strip_prefix('"')?;
        (rest.strip_suffix('"')?, '"', false)
    };
    if !body.is_empty()
        && body.trim() == body
        && !body.chars().any(char::is_control)
        && (here || !body.contains(quote))
        && (quote == '\'' || (!body.contains('`') && !body.contains('$')))
    {
        return Some(Cow::Borrowed(body));
    }
    let mut output = String::new();
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        let character = if quote == '"' && character == '`' {
            escaped(&mut characters)?
        } else if !here && character == quote {
            if characters.next()? != quote {
                return None;
            }
            quote
        } else {
            // The native selector excludes interpolation, but this shared helper
            // also rejects variable syntax if called without that proof.
            if quote == '"'
                && character == '$'
                && characters.clone().next().is_some_and(|next| {
                    next.is_alphanumeric() || matches!(next, '_' | '{' | '(' | '$' | '?' | '^')
                })
            {
                return None;
            }
            character
        };
        if output.len().checked_add(character.len_utf8())? > text.len() {
            return None;
        }
        output.try_reserve(character.len_utf8()).ok()?;
        output.push(character);
    }
    // Empty, control-bearing and boundary-whitespace keys keep an explicit
    // literal spelling instead of an invisible or misleading display label.
    if output.is_empty() || output.trim() != output || output.chars().any(char::is_control) {
        return None;
    }
    if output == body {
        Some(Cow::Borrowed(body))
    } else {
        Some(Cow::Owned(output))
    }
}

fn here_body<'a>(text: &'a str, closing: &str) -> Option<&'a str> {
    let (header, rest) = text.split_once('\n')?;
    if !header.chars().all(char::is_whitespace) {
        return None;
    }
    let body = rest.strip_suffix(closing)?.strip_suffix('\n')?;
    Some(body.strip_suffix('\r').unwrap_or(body))
}

fn escaped(characters: &mut Chars<'_>) -> Option<char> {
    Some(match characters.next()? {
        '0' => '\0',
        'a' => '\u{7}',
        'b' => '\u{8}',
        'e' => '\u{1b}',
        'f' => '\u{c}',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'v' => '\u{b}',
        'u' => {
            if characters.next()? != '{' {
                return None;
            }
            let mut scalar = 0u32;
            let mut digits = 0;
            loop {
                let character = characters.next()?;
                if character == '}' {
                    break;
                }
                if digits == 6 {
                    return None;
                }
                scalar = scalar
                    .checked_mul(16)?
                    .checked_add(character.to_digit(16)?)?;
                digits += 1;
            }
            if digits == 0 {
                return None;
            }
            char::from_u32(scalar)?
        }
        character => character,
    })
}

#[cfg(test)]
mod tests {
    use super::display_key;

    #[test]
    fn literal_key_display_decodes_without_replacing_written_identity() {
        for (source, expected) in [
            ("'quoted key'", "quoted key"),
            ("'can''t'", "can't"),
            ("\"a\"\"b\"", "a\"b"),
            ("\"`$key\"", "$key"),
            ("'literal $key'", "literal $key"),
            ("\"`u{96ea}\"", "雪"),
            ("\"`u{1f44d}\"", "👍"),
            ("\"a``b\"", "a`b"),
            ("@'\r\nhere key\r\n'@", "here key"),
            ("@\"\n`u{96ea}\n\"@", "雪"),
        ] {
            assert_eq!(display_key(source).as_deref(), Some(expected), "{source}");
            assert!(expected.len() <= source.len());
        }
    }

    #[test]
    fn ambiguous_or_unreadable_keys_keep_the_original_literal_display() {
        for source in [
            "Title",
            "''",
            "' '",
            "\"`0\"",
            "\"$key\"",
            "\"$(Read-Key)\"",
            "\"`u{}\"",
            "\"`u{110000}\"",
            "\"`u{d800}\"",
            "\"`u{1234567}\"",
            "'unfinished",
        ] {
            assert!(display_key(source).is_none(), "{source}");
        }
    }
}
